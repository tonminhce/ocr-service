import ast
import pathlib
import runpy
import shlex
import tempfile
import unittest

_MANIFEST = runpy.run_path(str(pathlib.Path(__file__).with_name("release-manifest.py")))
PIN = _MANIFEST["pin_image"]
VERIFY_RECEIPTS = _MANIFEST["verify_scan_receipts"]


class ImagePinTests(unittest.TestCase):
    def test_ci_preloads_required_runtime_image_aliases_before_inventory(self):
        root = pathlib.Path(__file__).resolve().parent.parent
        source = ast.parse((root / "scripts/release-manifest.py").read_text())
        images = next(node for node in ast.walk(source) if isinstance(node, ast.Dict)
                      and {"application", "postgres", "redis"}.issubset(
                          {key.value for key in node.keys if isinstance(key, ast.Constant)}))
        required = {value.value for value in images.values
                    if isinstance(value, ast.Constant) and isinstance(value.value, str)}
        self.assertTrue(required)
        workflow = (root / ".github/workflows/ci.yml").read_text()
        images_job = workflow.split("\n  images:\n", 1)[1]
        before_inventory = images_job[:images_job.index("      - name: Local release inventory and digest lock\n")]
        preload = before_inventory.split("      - name: Load runtime images for local inventory\n        run: |\n", 1)[1]
        preload = preload.split("\n      - ", 1)[0]
        loaded = set()
        for line in preload.splitlines():
            if not line.strip():
                continue
            command = shlex.split(line.strip())
            self.assertEqual(command[:2], ["docker", "pull"])
            self.assertEqual(len(command), 3)
            loaded.add(command[2])
        self.assertEqual(loaded, required)

    def test_runtime_images_are_digest_pinned(self):
        root = pathlib.Path(__file__).resolve().parent.parent
        source = ast.parse((root / "scripts/release-manifest.py").read_text())
        images = next(node for node in ast.walk(source) if isinstance(node, ast.Dict)
                      and {"application", "postgres", "redis"}.issubset(
                          {key.value for key in node.keys if isinstance(key, ast.Constant)}))
        for value in images.values:
            if isinstance(value, ast.Constant) and isinstance(value.value, str):
                self.assertIn("@sha256:", value.value, value.value)

    def test_ci_scan_loop_covers_every_inventoried_runtime_image(self):
        root = pathlib.Path(__file__).resolve().parent.parent
        source = ast.parse((root / "scripts/release-manifest.py").read_text())
        images = next(node for node in ast.walk(source) if isinstance(node, ast.Dict)
                      and {"application", "postgres", "redis"}.issubset(
                          {key.value for key in node.keys if isinstance(key, ast.Constant)}))
        required = {value.value for value in images.values
                    if isinstance(value, ast.Constant) and isinstance(value.value, str)}
        self.assertTrue(any("postgres" in value for value in required))
        workflow = (root / ".github/workflows/ci.yml").read_text()
        scan = workflow.split("SBOM and vulnerability scan", 1)[1]
        scan = scan[:scan.index("      - name: Load runtime images")]
        for value in required:
            if value.startswith("ocr-service:"):
                continue
            self.assertIn(value, scan, value)

    def test_never_substitutes_mutable_tag_for_missing_digest(self):
        identity = "sha256:" + "a" * 64
        self.assertEqual(PIN({"engine_image_id": identity, "repo_digests": ["image:latest"]}), (identity, "local_engine_identity"))

    def test_retains_repository_digest_and_rejects_invalid_identity(self):
        digest = "registry.example/ocr@sha256:" + "b" * 64
        self.assertEqual(PIN({"engine_image_id": "sha256:" + "a" * 64, "repo_digests": [digest]}), (digest, "repository_digest"))
        with self.assertRaises(ValueError):
            PIN({"engine_image_id": "image:latest", "repo_digests": [digest]})


class ScanReceiptTests(unittest.TestCase):
    @staticmethod
    def _images():
        return {
            "application": {"requested_reference": "ocr-service:abc"},
            "postgres": {"requested_reference": "postgres:18@sha256:" + "c" * 64},
        }

    def test_exact_receipt_set_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ["sbom-ocr-service-abc.cdx.json", "vulnerabilities-ocr-service-abc.txt",
                         "sbom-postgres-18-sha256-" + "c" * 64 + ".cdx.json",
                         "vulnerabilities-postgres-18-sha256-" + "c" * 64 + ".txt",
                         "image-ids.txt"]:
                (root / name).write_text("{}")
            VERIFY_RECEIPTS(self._images(), root)

    def test_missing_receipt_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ["sbom-ocr-service-abc.cdx.json", "vulnerabilities-ocr-service-abc.txt",
                         "sbom-postgres-18-sha256-" + "c" * 64 + ".cdx.json"]:
                (root / name).write_text("{}")
            with self.assertRaises(ValueError) as failure:
                VERIFY_RECEIPTS(self._images(), root)
            self.assertIn("missing receipts", str(failure.exception))

    def test_extra_receipt_without_deployed_image_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ["sbom-ocr-service-abc.cdx.json", "vulnerabilities-ocr-service-abc.txt",
                         "sbom-postgres-18-sha256-" + "c" * 64 + ".cdx.json",
                         "vulnerabilities-postgres-18-sha256-" + "c" * 64 + ".txt",
                         "sbom-ghost-image.cdx.json"]:
                (root / name).write_text("{}")
            with self.assertRaises(ValueError) as failure:
                VERIFY_RECEIPTS(self._images(), root)
            self.assertIn("receipts without a deployed image", str(failure.exception))


if __name__ == "__main__":
    unittest.main()
