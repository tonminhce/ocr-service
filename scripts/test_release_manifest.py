import ast
import pathlib
import runpy
import shlex
import unittest


PIN = runpy.run_path(str(pathlib.Path(__file__).with_name("release-manifest.py")))["pin_image"]


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

    def test_never_substitutes_mutable_tag_for_missing_digest(self):
        identity = "sha256:" + "a" * 64
        self.assertEqual(PIN({"engine_image_id": identity, "repo_digests": ["image:latest"]}), (identity, "local_engine_identity"))

    def test_retains_repository_digest_and_rejects_invalid_identity(self):
        digest = "registry.example/ocr@sha256:" + "b" * 64
        self.assertEqual(PIN({"engine_image_id": "sha256:" + "a" * 64, "repo_digests": [digest]}), (digest, "repository_digest"))
        with self.assertRaises(ValueError):
            PIN({"engine_image_id": "image:latest", "repo_digests": [digest]})


if __name__ == "__main__":
    unittest.main()
