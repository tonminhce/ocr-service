import pathlib
import runpy
import unittest


PIN = runpy.run_path(str(pathlib.Path(__file__).with_name("release-manifest.py")))["pin_image"]


class ImagePinTests(unittest.TestCase):
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
