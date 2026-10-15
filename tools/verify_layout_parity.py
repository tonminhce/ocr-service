"""Offline development QA for the course's Rust PP-DocLayoutV3 detector.

This tool is not part of the deployed application. It runs the unmodified
GLM-OCR SDK 0.1.4 detector from the locally extracted wheel and revision-pinned
cached model weights, then compares its output with the native Rust CLI. It
never invokes recognition, sends images to an API, or downloads model weights.

Usage (from the repository, with the export tool's development Python env):
    python tools/verify_layout_parity.py

Random resize fixtures and all reports remain under ignored artifacts/. Exact
label/task/order/coordinate equality is required; scores have one fixed
absolute epsilon. Differences are reported, never repaired by adjusting the
reference or silently expanding tolerances.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import importlib.metadata
import inspect
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import types
from typing import Any

# These are set before Transformers/Hugging Face are imported. All model
# resolution below also explicitly requests local_files_only.
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"

MODEL = "PaddlePaddle/PP-DocLayoutV3_safetensors"
REVISION = "e4f489dc536556fc1be02b973ba06756e0d4a2ac"
ROOT = Path(__file__).resolve().parents[1]
EXACT_FIELDS = ("index", "class_id", "label", "task_type", "reading_order", "bbox_2d", "polygon")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: Any) -> None:
    def convert(item: Any) -> Any:
        if hasattr(item, "tolist"):
            return item.tolist()
        raise TypeError(f"cannot serialize {type(item).__name__}")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, default=convert) + "\n")


def emit(value: Any) -> None:
    print(json.dumps(value, separators=(",", ":")), flush=True)


def run_native(args: argparse.Namespace, image: Path, tensor_output: Path | None = None) -> bytes:
    command = [str(args.binary), "--image", str(image), "--max-pixels", "71372800"]
    if tensor_output is not None:
        command += ["--tensor-output", str(tensor_output)]
    else:
        command += ["--model", str(args.model), "--sha256", args.model_sha256]
    environment = os.environ.copy()
    environment["ORT_DYLIB_PATH"] = str(args.ort_library)
    result = subprocess.run(command, env=environment, capture_output=True, timeout=args.timeout)
    if result.returncode:
        raise RuntimeError(f"native detector exited {result.returncode}: {result.stderr.decode(errors='replace')[:2000]}")
    return result.stdout


def load_reference(args: argparse.Namespace):
    import numpy as np
    import torch
    import yaml
    from huggingface_hub import snapshot_download

    # glmocr.utils.__init__ imports unrelated PDF-conversion locks, including
    # portalocker. Load its actual source submodules as a namespace package so
    # this layout-only QA does not need unrelated pipeline dependencies. The
    # detector, image processor, logging, visualization, and policy source are
    # imported and executed without editing or replacing their algorithms.
    sys.path.insert(0, str(args.sdk_source))
    sdk = importlib.import_module("glmocr")
    if sdk.__version__ != "0.1.4":
        raise RuntimeError("expected the extracted GLM-OCR SDK 0.1.4 source")
    utils = types.ModuleType("glmocr.utils")
    utils.__path__ = [str(args.sdk_source / "glmocr" / "utils")]
    utils.__package__ = "glmocr.utils"
    sys.modules["glmocr.utils"] = utils
    module = importlib.import_module("glmocr.layout.layout_detector")
    if Path(module.__file__).resolve() != (args.sdk_source / "glmocr/layout/layout_detector.py").resolve():
        raise RuntimeError("layout detector was not imported from the requested SDK source")
    LayoutConfig = importlib.import_module("glmocr.config").LayoutConfig
    layout = yaml.safe_load(args.course_config.read_text())["pipeline"]["layout"]
    source = args.model_source or Path(snapshot_download(MODEL, revision=REVISION, local_files_only=True))
    if not source.is_dir():
        raise RuntimeError("the revision-pinned reference model is not cached locally")
    config = LayoutConfig(**{**layout, "model_dir": str(source), "device": "cpu", "cuda_visible_devices": "", "batch_size": 1, "workers": 1})
    torch.set_num_threads(2)
    detector = module.PPDocLayoutDetector(config)
    detector.start()
    if detector._image_processor.size != {"height": 800, "width": 800}:
        raise RuntimeError("reference processor does not use the expected fixed 800x800 resize")
    captured: list[Any] = []
    original_policy = module.apply_layout_postprocess

    def capture_policy(*positional, **keyword):
        result = original_policy(*positional, **keyword)
        captured.extend(result)
        return result

    module.apply_layout_postprocess = capture_policy
    detector._qa_order_snapshot = {}
    original_order = detector._image_processor._get_order_seqs

    def capture_order(logits):
        ranks = original_order(logits)
        scores = torch.sigmoid(logits)
        votes = scores.triu(diagonal=1).sum(dim=1) + (1.0 - scores.transpose(1, 2)).tril(diagonal=-1).sum(dim=1)
        detector._qa_order_snapshot.clear()
        detector._qa_order_snapshot.update({"ranks": ranks[0].cpu().numpy().copy(),
                                           "votes": votes[0].cpu().numpy().copy()})
        return ranks

    detector._image_processor._get_order_seqs = capture_order
    return detector, captured, np, torch, source


def reference_canonical(detector, sdk_regions: list[dict], paddle_regions: list[dict]) -> tuple[list[dict], list[dict]]:
    label_to_id = {label: int(class_id) for class_id, label in detector.id2label.items()}
    retained_metadata = []
    abandoned = []
    for region in paddle_regions:
        task = next((name for name, labels in detector.label_task_mapping.items()
                     if isinstance(labels, list) and region["label"] in labels), None)
        if task is None:
            continue
        if task == "abandon":
            abandoned.append(region)
        else:
            retained_metadata.append(region)
    if len(sdk_regions) != len(retained_metadata):
        raise RuntimeError("SDK metadata observer lost alignment with SDK final output")
    canonical = []
    for region, metadata in zip(sdk_regions, retained_metadata):
        canonical.append({**region, "class_id": label_to_id[region["label"]],
                          "reading_order": 0 if metadata["order"] is None else int(metadata["order"])})
    return canonical, abandoned


def native_canonical(native: dict) -> list[dict]:
    return [{"index": region["index"], "class_id": region["class_id"], "label": region["label"],
             "task_type": region["task"], "reading_order": region["reading_order"],
             "bbox_2d": region["bbox_2d"], "polygon": region["polygon"], "score": region["score"]}
            for region in native["regions"]]


def compare_regions(expected: list[dict], actual: list[dict], score_epsilon: float) -> dict:
    differences = []
    if len(expected) != len(actual):
        differences.append({"field": "region_count", "reference": len(expected), "native": len(actual)})
    max_score_error = 0.0
    for index, (reference, native) in enumerate(zip(expected, actual)):
        for field in EXACT_FIELDS:
            if reference[field] != native[field]:
                differences.append({"region": index, "field": field, "reference": reference[field], "native": native[field]})
        score_error = abs(float(reference["score"]) - float(native["score"]))
        max_score_error = max(max_score_error, score_error)
        if score_error > score_epsilon:
            differences.append({"region": index, "field": "score", "reference": reference["score"],
                                "native": native["score"], "absolute_error": score_error})
    return {"passed": not differences, "strict_numerical_passed": not differences,
            "layout_contract_passed": not any(difference["field"] != "reading_order" for difference in differences),
            "reference_regions": len(expected), "native_regions": len(actual),
            "maximum_score_absolute_error": max_score_error, "differences": differences}


def verify_preprocessing(args: argparse.Namespace, detector, np) -> list[dict]:
    from PIL import Image

    cases = []
    # Tuples are (height, width), matching export_pp_doclayout_v3.py.
    for number, (height, width) in enumerate([(960, 720), (480, 800), (800, 800), (1, 1), (17, 29)]):
        raw = np.random.default_rng(42 + number).integers(0, 256, (height, width, 3), dtype=np.uint8)
        name = f"resize-h{height}-w{width}"
        input_path = args.output_dir / "preprocessing" / f"{name}.png"
        input_path.parent.mkdir(parents=True, exist_ok=True)
        Image.fromarray(raw).save(input_path)
        native_path = input_path.with_suffix(".native.rgb")
        run_native(args, input_path, native_path)
        native_bytes = native_path.read_bytes()
        tensor = detector._image_processor(images=raw, input_data_format="channels_last",
                                           do_rescale=False, do_normalize=False,
                                           return_tensors="pt")["pixel_values"][0]
        if str(tensor.dtype) != "torch.uint8" or tuple(tensor.shape) != (3, 800, 800):
            raise RuntimeError("official resize observer did not return uint8 RGB at 800x800")
        reference = tensor.permute(1, 2, 0).contiguous().numpy()
        reference_bytes = reference.tobytes()
        input_path.with_suffix(".reference.rgb").write_bytes(reference_bytes)
        same_length = len(native_bytes) == len(reference_bytes)
        delta = (np.frombuffer(native_bytes, np.uint8).astype(np.int16) - reference.reshape(-1).astype(np.int16)) if same_length else None
        result = {"height": height, "width": width, "passed": native_bytes == reference_bytes,
                  "reference_sha256": hashlib.sha256(reference_bytes).hexdigest(),
                  "native_sha256": hashlib.sha256(native_bytes).hexdigest(),
                  "reference_bytes": len(reference_bytes), "native_bytes": len(native_bytes),
                  "different_bytes": int(np.count_nonzero(delta)) if delta is not None else None,
                  "maximum_byte_absolute_error": int(np.max(np.abs(delta))) if delta is not None else None}
        cases.append(result)
        emit({"stage": "preprocessing", **{key: result[key] for key in ("height", "width", "passed", "different_bytes", "maximum_byte_absolute_error")}})
    return cases


def diagnose_near_ties(outputs, native, ranks, votes, np, torch) -> dict:
    """Show whether numeric rank changes cross retained or discarded queries."""
    boxes = outputs["pred_boxes"][0]
    predicted = np.concatenate((boxes[:, :2] - np.float32(0.5) * boxes[:, 2:],
                                boxes[:, :2] + np.float32(0.5) * boxes[:, 2:]), axis=1)
    predicted *= np.asarray([native["width"], native["height"], native["width"], native["height"]], np.float32)
    queries = []
    errors = []
    for region in native["regions"]:
        distances = np.max(np.abs(predicted - np.asarray(region["bbox"], np.float32)), axis=1)
        query = int(np.argmin(distances))
        queries.append(query)
        errors.append(float(distances[query]))
    retained = set(queries)
    pointers = np.argsort(ranks)
    differences = []
    for index, (region, query) in enumerate(zip(native["regions"], queries)):
        expected_rank = int(ranks[query])
        actual_rank = int(region["reading_order"])
        if expected_rank == actual_rank:
            continue
        crossed = []
        low, high = sorted((expected_rank, actual_rank))
        for other in pointers[low:high + 1]:
            other = int(other)
            if other == query:
                continue
            gap = abs(float(votes[query]) - float(votes[other]))
            crossed.append({"query": other, "retained": other in retained,
                            "vote": float(votes[other]), "absolute_vote_gap": gap,
                            "vote_gap_in_float32_ulps": gap / float(np.spacing(votes[query]))})
        differences.append({"region": index, "query": query, "sdk_on_onnx_rank": expected_rank,
                            "native_rank": actual_rank, "query_vote": float(votes[query]),
                            "bbox_query_match_maximum_error_pixels": errors[index],
                            "crossed_queries": crossed,
                            "all_crossed_queries_unretained": all(not other["retained"] for other in crossed)})

    # Diagnose summation alone with the official sigmoid values. This is an
    # explicit float32 fold, matching Rust's separate before/after loops; its
    # purpose is explanatory, not an alternative reference implementation.
    scores = torch.sigmoid(torch.from_numpy(outputs["order_logits"]))[0].numpy()
    emulated_votes = np.zeros(300, np.float32)
    for query in range(300):
        before = np.float32(0)
        after = np.float32(0)
        for earlier in range(query):
            before = np.float32(before + scores[earlier, query])
        for later in range(query + 1, 300):
            after = np.float32(after + np.float32(1 - scores[query, later]))
        emulated_votes[query] = np.float32(before + after)
    emulated_ranks = np.empty(300, np.int64)
    emulated_ranks[np.argsort(emulated_votes, kind="stable")] = np.arange(300)
    matched = sum(int(emulated_ranks[query]) == region["reading_order"] for query, region in zip(queries, native["regions"]))
    return {"retained_region_order_unchanged": queries == sorted(queries, key=lambda query: int(ranks[query])),
            "all_rank_crossings_are_unretained_queries": all(item["all_crossed_queries_unretained"] for item in differences),
            "rank_crossings": differences,
            "float32_fold_using_official_sigmoid": {"matched_native_retained_ranks": matched,
                                                    "retained_regions": len(queries),
                                                    "maximum_vote_absolute_error_from_torch_reduction": float(np.max(np.abs(emulated_votes - votes)))}}


def diagnose_with_onnx_outputs(args, name, detector, captured, np, torch, page, native, session, original_orders) -> dict:
    """Locate model-export versus Rust-policy differences without changing QA.

    This separate diagnostic feeds native ONNX output through the unchanged
    SDK processor/policy. Its outcome never substitutes for the real SDK
    reference comparison above.
    """
    tensors = detector._image_processor(images=[page], return_tensors="pt")
    values = session.run(None, {"pixel_values": tensors["pixel_values"].numpy()})
    outputs = dict(zip([output.name for output in session.get_outputs()], values))
    np.save(args.output_dir / "pages" / f"{name}.onnx-order-logits.npy", outputs["order_logits"])
    orders = torch.from_numpy(outputs["order_logits"])
    scores = torch.sigmoid(orders)
    votes = scores.triu(diagonal=1).sum(dim=1) + (1.0 - scores.transpose(1, 2)).tril(diagonal=-1).sum(dim=1)
    ranks = detector._image_processor._get_order_seqs(orders)
    write_json(args.output_dir / "pages" / f"{name}.onnx-order-reference.json",
               {"votes": votes[0].tolist(), "ranks": ranks[0].tolist()})

    class NativeOutputAdapter:
        def __call__(self, **unused):
            return types.SimpleNamespace(**{key: torch.from_numpy(value.copy()) for key, value in outputs.items()})

    original_model = detector._model
    try:
        detector._model = NativeOutputAdapter()
        captured.clear()
        with torch.inference_mode():
            sdk_output = detector.process([page])[0]
        expected, _ = reference_canonical(detector, sdk_output, captured[0])
    finally:
        detector._model = original_model
    write_json(args.output_dir / "pages" / f"{name}.sdk-on-onnx.json", expected)
    result = compare_regions(expected, native_canonical(native), args.score_epsilon)
    changed_queries = np.flatnonzero(ranks[0].numpy() != original_orders["ranks"])
    result["full_query_rank_changes_from_model_export"] = len(changed_queries)
    result["model_export_rank_changes"] = [
        {"query": int(query), "pytorch_rank": int(original_orders["ranks"][query]),
         "onnx_rank": int(ranks[0, query]),
         "retained_by_sdk": int(original_orders["ranks"][query]) in original_orders["retained_ranks"]}
        for query in changed_queries
    ]
    result["near_tie_evidence"] = diagnose_near_ties(outputs, native, ranks[0].numpy(), votes[0].numpy(), np, torch)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-source", type=Path, default=ROOT / "artifacts/research/glm-ocr-sdk/wheel-0.1.4")
    parser.add_argument("--course-config", type=Path, default=ROOT.parent / "production-ocr-course/client_rt_consumer/config.yaml")
    parser.add_argument("--model", type=Path, default=ROOT / "artifacts/models/pp-doclayout-v3/model.onnx")
    parser.add_argument("--model-source", type=Path, help="explicit local snapshot of the pinned reference weights")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/layout-detect")
    parser.add_argument("--ort-library", type=Path)
    parser.add_argument("--public-fixture", type=Path, default=ROOT / "artifacts/layout-validation/public/fixture-page.png")
    parser.add_argument("--private-dir", type=Path, default=ROOT / "artifacts/user-pdf")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "artifacts/layout-validation")
    parser.add_argument("--score-epsilon", type=float, default=0.0001)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--public-only", action="store_true")
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    report_path = args.output_dir / "parity-report.json"
    report: dict[str, Any] = {"schema_version": 1, "status": "running", "offline": True,
                              "reference_sdk": "0.1.4", "model": MODEL, "revision": REVISION,
                              "exact_fields": EXACT_FIELDS, "score_absolute_epsilon": args.score_epsilon,
                              "layout_contract_exact_fields": [field for field in EXACT_FIELDS if field != "reading_order"],
                              "strict_additional_metadata_fields": ["reading_order"],
                              "order_contract": "SDK output order is its contiguous index sequence; full 300-query reading_order rank is additional native metadata and remains a separate strict test.",
                              "preprocessing": [], "pages": []}
    detector = None
    try:
        if not 0 <= args.score_epsilon <= 0.0001:
            raise ValueError("score epsilon must be at most this tool's fixed 0.0001 limit")
        import cv2
        import onnxruntime

        if args.ort_library is None:
            capi = Path(onnxruntime.__file__).parent / "capi"
            candidates = list(capi.glob("libonnxruntime.*.dylib")) + list(capi.glob("libonnxruntime.so.*"))
            if len(candidates) != 1:
                raise RuntimeError("specify --ort-library: could not identify exactly one native ORT library")
            args.ort_library = candidates[0]
        for name in ("binary", "model", "ort_library", "course_config", "public_fixture"):
            if not getattr(args, name).is_file():
                raise FileNotFoundError(f"required local {name} is unavailable")
        args.model_sha256 = sha256_file(args.model)
        # Freeze the executable used by this run. A concurrent cargo rebuild
        # must not mix native versions among the five page comparisons.
        snapshot = args.output_dir / "runtime" / f"layout-detect-{os.getpid()}"
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(args.binary, snapshot)
        args.binary = snapshot
        report.update({"onnx_sha256": args.model_sha256, "onnx_bytes": args.model.stat().st_size,
                       "course_config_sha256": sha256_file(args.course_config),
                       "native_binary_sha256": sha256_file(args.binary),
                       "runtime_library": args.ort_library.name,
                       "versions": {name: importlib.metadata.version(name) for name in ("torch", "transformers", "onnxruntime")},
                       "opencv_version": cv2.__version__})
        detector, captured, np, torch, source = load_reference(args)
        report["reference_source"] = {"weights_snapshot": source.name,
                                      "sdk_detector_sha256": sha256_file(args.sdk_source / "glmocr/layout/layout_detector.py"),
                                      "sdk_policy_sha256": sha256_file(args.sdk_source / "glmocr/utils/layout_postprocess_utils.py"),
                                      "processor_sha256": sha256_file(Path(inspect.getfile(type(detector._image_processor)))),
                                      "import_mode": "unmodified source modules, unrelated utility package initializer skipped"}
        report["preprocessing"] = verify_preprocessing(args, detector, np)
        write_json(report_path, report)
        from PIL import Image

        images = [("public-fixture", args.public_fixture)]
        if not args.public_only:
            images += [(f"private-page-{page}", args.private_dir / f"reference-page-{page}.png") for page in range(1, 5)]
        diagnostic_session = None
        for name, path in images:
            started = time.monotonic()
            result: dict[str, Any] = {"name": name, "private": name.startswith("private"), "passed": False,
                                      "layout_contract_passed": False, "strict_numerical_passed": False}
            try:
                captured.clear()
                with Image.open(path) as opened:
                    page = opened.convert("RGB")
                result.update({"width": page.width, "height": page.height, "input_sha256": sha256_file(path)})
                with torch.inference_mode():
                    sdk_output = detector.process([page])[0]
                if len(captured) != 1:
                    raise RuntimeError("SDK policy observer expected exactly one page")
                original_orders = dict(detector._qa_order_snapshot)
                expected, abandoned = reference_canonical(detector, sdk_output, captured[0])
                original_orders["retained_ranks"] = [region["reading_order"] for region in expected]
                native = json.loads(run_native(args, path))
                write_json(args.output_dir / "pages" / f"{name}.sdk.json", sdk_output)
                write_json(args.output_dir / "pages" / f"{name}.sdk-paddle.json", captured[0])
                write_json(args.output_dir / "pages" / f"{name}.native.json", native)
                write_json(args.output_dir / "pages" / f"{name}.sdk-order-reference.json", original_orders)
                result.update(compare_regions(expected, native_canonical(native), args.score_epsilon))
                result["reference_abandoned_regions"] = len(abandoned)
                result["native_abandoned_regions"] = len(native["discarded"])
                if (native["width"], native["height"]) != (page.width, page.height):
                    result["passed"] = False
                    result["layout_contract_passed"] = False
                    result["strict_numerical_passed"] = False
                    result["differences"].append({"field": "page_dimensions"})
                if not result["passed"]:
                    if diagnostic_session is None:
                        options = onnxruntime.SessionOptions()
                        options.intra_op_num_threads = 2
                        options.inter_op_num_threads = 1
                        diagnostic_session = onnxruntime.InferenceSession(str(args.model), sess_options=options,
                                                                          providers=["CPUExecutionProvider"])
                    result["sdk_on_onnx_diagnostic"] = diagnose_with_onnx_outputs(
                        args, name, detector, captured, np, torch, page, native, diagnostic_session, original_orders)
            except Exception as error:
                result["error"] = f"{type(error).__name__}: {error}"
            result["elapsed_seconds"] = round(time.monotonic() - started, 3)
            report["pages"].append(result)
            write_json(report_path, report)
            emit({"stage": "page", "name": name, "passed": result["passed"],
                  "layout_contract_passed": result["layout_contract_passed"],
                  "reference_regions": result.get("reference_regions"), "native_regions": result.get("native_regions"),
                  "difference_fields": sorted({difference["field"] for difference in result.get("differences", [])}),
                  "maximum_score_absolute_error": result.get("maximum_score_absolute_error"),
                  "sdk_on_onnx_passed": result.get("sdk_on_onnx_diagnostic", {}).get("passed"),
                  "error": result.get("error")})
        passed = all(case["passed"] for case in report["preprocessing"] + report["pages"])
        report["status"] = "passed" if passed else "failed"
        contract_passed = all(case["passed"] for case in report["preprocessing"]) and all(page["layout_contract_passed"] for page in report["pages"])
        report["layout_contract_status"] = "passed" if contract_passed else "failed"
        report["strict_numerical_status"] = report["status"]
        write_json(report_path, report)
        emit({"status": report["status"], "report": str(report_path)})
        return 0 if passed else 1
    except Exception as error:
        report.update({"status": "error", "error": f"{type(error).__name__}: {error}"})
        write_json(report_path, report)
        emit({"status": "error", "error": report["error"], "report": str(report_path)})
        return 2
    finally:
        if detector is not None:
            detector.stop()


if __name__ == "__main__":
    raise SystemExit(main())
