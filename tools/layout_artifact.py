"""Offline setup helper: verify model and update only its deployment settings."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import tempfile

MODEL = "PaddlePaddle/PP-DocLayoutV3_safetensors"
REVISION = "e4f489dc536556fc1be02b973ba06756e0d4a2ac"


def verified(directory):
    manifest_path = directory / "manifest.json"
    model_path = directory / "model.onnx"
    if manifest_path.stat().st_size > 65536 or model_path.stat().st_size > 256 * 1024 * 1024:
        raise ValueError("artifact size bound exceeded")
    manifest = json.loads(manifest_path.read_text())
    assert manifest["model"] == MODEL and manifest["revision"] == REVISION
    assert manifest["input"]["shape"] == [1, 3, 800, 800]
    assert manifest["validation"] and all(v["ordered_labels_equal"] and v["preprocessing_exact"] for v in manifest["validation"])
    with model_path.open("rb") as model:
        digest = hashlib.file_digest(model, "sha256").hexdigest()
    assert manifest["onnx_sha256"] == digest and manifest["onnx_bytes"] == model_path.stat().st_size
    return digest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["verify", "configure"])
    parser.add_argument("directory", type=Path)
    parser.add_argument("--env-file", type=Path, default=Path(".env"))
    args = parser.parse_args()
    try:
        directory = args.directory.resolve()
        digest = verified(directory)
    except (OSError, ValueError, KeyError, AssertionError):
        raise SystemExit("No verified PP-DocLayoutV3 artifact; export is required.")
    if args.mode == "verify":
        print(f"Verified layout artifact SHA-256: {digest}")
        return
    values = {"LAYOUT_MODEL_SHA256": digest, "LAYOUT_MODEL_DIR": str(directory)}
    if any("\n" in value or "\r" in value for value in values.values()):
        raise SystemExit("Model directory cannot contain line breaks")
    # Do not print, replace, or regenerate any application credentials.
    lines = args.env_file.read_text().splitlines() if args.env_file.exists() else []
    lines = [line for line in lines if not any(line.startswith(key + "=") for key in values)]
    lines += [key + "=" + shlex.quote(value) for key, value in values.items()]
    parent = args.env_file.resolve().parent
    with tempfile.NamedTemporaryFile(mode="w", dir=parent, delete=False) as output:
        output.write("\n".join(lines) + "\n")
        temporary = output.name
    os.chmod(temporary, 0o600)
    os.replace(temporary, args.env_file)
    print(f"Configured layout artifact in {args.env_file}; existing credentials preserved.")


if __name__ == "__main__":
    main()
