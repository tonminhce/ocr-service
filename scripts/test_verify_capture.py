import json
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import verify_capture
from verify_capture import CaptureError, parse_single_json, run_captured, scan_secrets


class SingleJsonTests(unittest.TestCase):
    def test_exact_single_json_document_parses(self):
        value = parse_single_json(json.dumps({"ok": True}).encode())
        self.assertEqual(value, {"ok": True})

    def test_trailing_garbage_is_rejected(self):
        with self.assertRaises(CaptureError):
            parse_single_json(b'{"ok": true} trailing')

    def test_prefix_noise_is_rejected(self):
        with self.assertRaises(CaptureError):
            parse_single_json(b'prefix {"ok": true}')

    def test_two_documents_are_rejected(self):
        with self.assertRaises(CaptureError):
            parse_single_json(b'{"a": 1} {"b": 2}')

    def test_empty_stream_is_rejected(self):
        with self.assertRaises(CaptureError):
            parse_single_json(b"")


class SecretScanTests(unittest.TestCase):
    def test_synthetic_hex64_token_is_flagged(self):
        with self.assertRaises(CaptureError) as failure:
            scan_secrets(b"token=" + b"0123456789abcdef" * 4)
        self.assertIn("synthetic-64-hex-token", str(failure.exception))

    def test_bearer_credential_is_flagged(self):
        with self.assertRaises(CaptureError) as failure:
            scan_secrets(b"Authorization: Bearer abcdef1234567890abcdef12345678901234")
        self.assertIn("bearer-credential", str(failure.exception))

    def test_clean_output_passes(self):
        scan_secrets(b"running check 1/18: passed")

    def test_allow_hex64_escapes_only_the_hex_rule(self):
        scan_secrets(b"a" * 64, allow_hex64=True)
        with self.assertRaises(CaptureError):
            scan_secrets(b"Bearer " + b"a" * 64, allow_hex64=True)


class RunCapturedTests(unittest.TestCase):
    def test_stdout_stderr_returncode_and_hashes(self):
        capture = run_captured(
            [sys.executable, "-c",
             "import sys; sys.stdout.write('{\"ok\": 1}'); sys.stderr.write('note')"],
            timeout=10)
        self.assertEqual(capture.returncode, 0)
        self.assertEqual(capture.stdout, b'{"ok": 1}')
        self.assertEqual(capture.stderr, b"note")
        self.assertFalse(capture.stdout_truncated)
        self.assertEqual(capture.stdout_sha256,
                         verify_capture.hashlib.sha256(b'{"ok": 1}').hexdigest())

    def test_output_flood_is_capped_and_flagged(self):
        capture = run_captured(
            [sys.executable, "-c", "print('x' * 5000000)"],
            timeout=30, stdout_cap=1024)
        self.assertTrue(capture.stdout_truncated)
        self.assertEqual(len(capture.stdout), 1024)

    def test_nonzero_exit_propagates(self):
        capture = run_captured([sys.executable, "-c", "raise SystemExit(3)"], timeout=10)
        self.assertEqual(capture.returncode, 3)

    def test_timeout_kills_and_raises(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            run_captured([sys.executable, "-c", "import time; time.sleep(60)"], timeout=1)


class SnapshotTests(unittest.TestCase):
    def test_snapshot_parses_compose_and_docker_output(self):
        def fake_run(command, **kwargs):
            if command[1] == "compose":
                return subprocess.CompletedProcess(command, 0, stdout=b'[{"ID": "c1"}]', stderr=b"")
            if command[1] == "volume":
                return subprocess.CompletedProcess(command, 0, stdout=b"v1\nv2\n", stderr=b"")
            return subprocess.CompletedProcess(command, 0, stdout=b"n1\n", stderr=b"")
        original = verify_capture.subprocess.run
        verify_capture.subprocess.run = fake_run
        try:
            snapshot = verify_capture.preexisting_snapshot("proj")
        finally:
            verify_capture.subprocess.run = original
        self.assertEqual(snapshot, {"containers": ["c1"], "volumes": ["v1", "v2"], "networks": ["n1"]})

    def test_postflight_detects_a_changed_original(self):
        calls = {"count": 0}

        def fake_run(command, **kwargs):
            if command[1] == "compose":
                calls["count"] += 1
                stdout = b'[{"ID": "c1"}]' if calls["count"] == 1 else b'[{"ID": "c1"}, {"ID": "c9"}]'
                return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr=b"")
            return subprocess.CompletedProcess(command, 0, stdout=b"", stderr=b"")
        original = verify_capture.subprocess.run
        verify_capture.subprocess.run = fake_run
        try:
            snapshot = verify_capture.preexisting_snapshot("proj")
            with self.assertRaises(CaptureError):
                verify_capture.assert_originals_unchanged("proj", snapshot)
        finally:
            verify_capture.subprocess.run = original


if __name__ == "__main__":
    unittest.main()
