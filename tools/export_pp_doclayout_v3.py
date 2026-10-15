"""One-time model export tool; never used by the Rust application at runtime.

Exports the official course detector to a portable ONNX graph. Rust implements
its uint8 bicubic preprocessing. Runtime inference, postprocessing, crops and OCR
orchestration belong to Rust. This tool downloads public model weights only.
"""

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import onnx
import onnxruntime as ort
import torch
import torch.nn.functional as F
from PIL import Image, ImageDraw, ImageFont
from transformers import AutoImageProcessor, AutoModelForObjectDetection

MODEL = "PaddlePaddle/PP-DocLayoutV3_safetensors"
REVISION = "e4f489dc536556fc1be02b973ba06756e0d4a2ac"


class DetectorGraph(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.detector = model

    def forward(self, image):
        # Rust reproduces the upstream uint8 separable bicubic kernel. ORT's
        # Resize CPU kernel cannot execute cubic uint8 input on this platform.
        result = self.detector(pixel_values=image)
        return result.logits, result.pred_boxes, result.order_logits, result.out_masks


class FixedPositionEmbedding(torch.nn.Module):
    """Freeze the exact official 25x25 embedding for fixed 800px model input.

    The upstream computes CPU sine/cosine in float64, which ORT's arm64 CPU
    build does not implement. A constant preserves its exact float32 result.
    """
    def __init__(self, source):
        super().__init__()
        with torch.no_grad():
            value = source(25, 25, torch.device("cpu"), torch.float32)
        self.register_buffer("value", value)

    def forward(self, width, height, device, dtype):
        return self.value.to(device=device, dtype=dtype)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", type=Path, default=Path("artifacts/models/pp-doclayout-v3"))
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    torch.set_num_threads(2)
    model = AutoModelForObjectDetection.from_pretrained(
        MODEL, revision=REVISION, trust_remote_code=False,
        disable_custom_kernels=True, local_files_only=True, dtype=torch.float32,
    ).cpu().eval()
    for layer in model.model.encoder.aifi:
        layer.position_embedding = FixedPositionEmbedding(layer.position_embedding)
    processor = AutoImageProcessor.from_pretrained(MODEL, revision=REVISION, local_files_only=True)
    graph = DetectorGraph(model).eval()
    path = args.output_dir / "model.onnx"
    sample = torch.zeros((1, 3, 800, 800), dtype=torch.float32)
    with torch.inference_mode():
        torch.onnx.export(
            graph, (sample,), str(path), input_names=["pixel_values"],
            output_names=["logits", "pred_boxes", "order_logits", "out_masks"],
            opset_version=17, dynamo=False, external_data=False,
        )
    onnx.checker.check_model(str(path))
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(str(path), sess_options=options, providers=["CPUExecutionProvider"])
    comparisons = []
    for height, width in [(960, 720), (480, 800)]:
        image = Image.new("RGB", (width, height), "white")
        drawing = ImageDraw.Draw(image)
        font = ImageFont.load_default(size=24)
        for row, text in enumerate(["OCR SERVICE TEST", "Document parsing reference", "Agreement number: 000123456789", "Amount: 123456", "Date: 15/10/2026", "Signature: example only"]):
            drawing.text((30, 25 + row * 60), text, fill="black", font=font)
        raw = np.array(image).transpose(2, 0, 1)[None]
        official = processor(images=image, return_tensors="pt")["pixel_values"]
        with torch.inference_mode():
            expected = [x.numpy() for x in graph(official)]
        actual = session.run(None, {"pixel_values": official.numpy()})
        deltas = [float(np.max(np.abs(a - e))) for a, e in zip(actual, expected)]
        names = ["logits", "pred_boxes", "order_logits", "out_masks"]
        reference = SimpleNamespace(**dict(zip(names, map(torch.from_numpy, expected))))
        converted = SimpleNamespace(**dict(zip(names, map(torch.from_numpy, actual))))
        ref_regions = processor.post_process_object_detection(reference, threshold=0.3, target_sizes=[(height,width)])[0]
        ort_regions = processor.post_process_object_detection(converted, threshold=0.3, target_sizes=[(height,width)])[0]
        assert len(ref_regions["labels"]) > 0, "document validation fixture must produce detections"
        np.testing.assert_array_equal(ref_regions["labels"].numpy(), ort_regions["labels"].numpy())
        np.testing.assert_allclose(ref_regions["scores"].numpy(), ort_regions["scores"].numpy(), rtol=0, atol=0.0001)
        np.testing.assert_allclose(ref_regions["boxes"].numpy(), ort_regions["boxes"].numpy(), rtol=0, atol=0.1)
        assert len(ref_regions["polygon_points"]) == len(ort_regions["polygon_points"])
        for ref_poly, ort_poly in zip(ref_regions["polygon_points"], ort_regions["polygon_points"]):
            np.testing.assert_allclose(ref_poly, ort_poly, rtol=0, atol=0.1)
        # Independently confirm the wrapper matches the official preprocessing.
        resized = F.interpolate(torch.from_numpy(raw), (800, 800), mode="bicubic", align_corners=False)
        preprocessed = resized.float() / 255.0
        np.testing.assert_array_equal(preprocessed.numpy(), official.numpy())
        comparisons.append({"height":height,"width":width,"regions":len(ref_regions["labels"]),"ordered_labels_equal":True,"box_and_polygon_tolerance_pixels":0.1,"max_raw_absolute_errors":dict(zip(names,deltas)),"preprocessing_exact":True})
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    manifest = {
        "schema_version":1,"model":MODEL,"revision":REVISION,"onnx_sha256":digest,
        "onnx_bytes":path.stat().st_size,"opset":17,
        "input":{"name":"pixel_values","dtype":"float32","shape":[1,3,800,800],"color":"RGB"},
        "outputs":["logits","pred_boxes","order_logits","out_masks"],
        "preprocessing":"Rust upstream uint8 separable bicubic 800x800, antialias=false, RGB /255, mean=0 std=1",
        "profile":"glmocr SDK 0.1.4 course layout, learned order, mask polygons",
        "position_embedding":"official CPU float64 arithmetic frozen as exact float32 25x25 constant",
        "validation_note":"semantic ordered regions compared; raw background queries can permute under near-tied encoder TopK scores",
        "id2label":model.config.id2label,
        "export_versions":{k:importlib.metadata.version(k) for k in ["torch","transformers","onnx","onnxruntime"]},
        "validation":comparisons,
    }
    (args.output_dir / "manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
    print(json.dumps({"path":str(path),"sha256":digest,"validation":comparisons},indent=2))


if __name__ == "__main__":
    main()
