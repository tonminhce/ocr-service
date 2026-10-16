import copy
import os
import pathlib
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


MODULE = runpy.run_path(str(pathlib.Path(__file__).with_name("parser-boundary-smoke.py")))
Harness = MODULE["Harness"]


def result(code=0, stdout=b""):
    return subprocess.CompletedProcess([], code, stdout, b"")


class BoundaryTests(unittest.TestCase):
    def test_compose_security_drift_is_rejected(self):
        source = (pathlib.Path(__file__).resolve().parent.parent / "compose.production.yaml").read_text()
        MODULE["require_policy"](source)
        for changed in [source.replace("network_mode: none", "network_mode: bridge"),
                        source.replace("    cpus: 2", "    cpus: 3"),
                        source.replace("    cpus: 2", "    cpus: 2.5"),
                        source.replace("      - parser-socket-data:/run/ocr-parser\n    tmpfs:", "      - parser-socket-data:/run/ocr-parser\n      - /sensitive:/sensitive\n    tmpfs:"),
                        source.replace("    cpus: 2", "    secrets: [production]\n    cpus: 2"),
                        source.replace("    cpus: 2", "    cap_add: [NET_ADMIN]\n    cpus: 2")]:
            with self.assertRaises(ValueError):
                MODULE["require_policy"](changed)

    def test_timeouts_missing_tools_and_missing_positive_controls_are_not_denial(self):
        MODULE["classify_probe"](result(42, b"connect_denied"), 42, b"connect_denied")
        for code in [0, 1, 124, 126, 127]:
            with self.assertRaises(ValueError):
                MODULE["classify_probe"](result(code, b"connect_denied"), 42, b"connect_denied")
        with self.assertRaises(ValueError):
            MODULE["classify_probe"](result(42), 42, b"connect_denied")
        checks = MODULE["REQUIRED_CHECKS"]
        MODULE["finish_checks"](list(checks))
        for omitted in ["canary_positive_before", "canary_positive_after", "network_denial"]:
            with self.assertRaises(ValueError):
                MODULE["finish_checks"](list(checks - {omitted}))

    def test_observed_network_mount_resource_drift_fails(self):
        image = "sha256:" + "a" * 64
        valid = {"image": image, "user": "10002:10001", "network": "none", "readonly": True,
                 "caps": ["ALL"], "security": ["no-new-privileges:true"], "memory": 2 * 1024 ** 3,
                 "swap": 4 * 1024 ** 3, "cpus": 2_000_000_000, "pids": 64, "init": True, "ports": {},
                 "tmpfs": {"/scratch": "size=1g,mode=0700,uid=10002,gid=10002,noexec,nosuid,nodev"},
                 "mounts": [{"destination": "/run/ocr-parser", "type": "volume", "rw": True}]}
        MODULE["verify_parser"](valid, image)
        for key, value in [("network", "bridge"), ("readonly", False), ("memory", 0), ("swap", 0),
                           ("caps", []), ("ports", {"8000/tcp": []}), ("pids", 0),
                           ("tmpfs", {"/scratch": "size=1g,mode=0700,uid=10002,gid=10002,exec,nosuid,nodev"}),
                           ("mounts", valid["mounts"] + [{"destination": "/run/secrets", "type": "bind", "rw": False}])]:
            changed = copy.deepcopy(valid)
            changed[key] = value
            with self.assertRaises(ValueError):
                MODULE["verify_parser"](changed, image)

    def test_foreign_named_volume_or_container_is_refused_without_mutation(self):
        for collision in ["volume", "container", "label"]:
            harness = Harness(pathlib.Path("/nonexistent"), "rust-ocr-unit-smoke")
            def fake(arguments, **kwargs):
                if collision == "label" and "--filter" in arguments:
                    return result(stdout=b"foreign")
                if arguments[:2] == ["volume", "inspect"] and collision == "volume":
                    self.assertEqual(arguments[3], "{{.Name}}")
                    return result(stdout=b"foreign-volume")
                if "inspect" in arguments:
                    return result(1)
                if "{{.Names}}" in arguments and collision == "container":
                    return result(stdout=b"rust-ocr-unit-smoke-parser")
                return result()
            with mock.patch.object(harness, "call", side_effect=fake) as command:
                with self.assertRaises(ValueError):
                    harness.refuse_existing()
                self.assertFalse(any("create" in call.args[0] for call in command.call_args_list))

    def test_failed_create_is_not_owned_and_cleanup_uses_ids_and_owner_uuid(self):
        harness = Harness(pathlib.Path("/nonexistent"), "rust-ocr-unit-smoke")
        self.assertNotEqual(harness.owner, Harness(harness.output, harness.project).owner)
        with mock.patch.object(harness, "call", side_effect=ValueError("collision")):
            with self.assertRaises(ValueError):
                harness.start("parser", "sha256:" + "a" * 64, [])
        self.assertEqual(harness.containers, [])
        identity = "b" * 64
        with mock.patch.object(harness, "call", return_value=result(stdout=identity.encode())):
            self.assertEqual(harness.start("parser", "sha256:" + "a" * 64, []), identity)
        self.assertEqual(harness.containers, [identity])
        with mock.patch.object(harness, "call", return_value=result()) as command:
            harness.exec("parser", "exit 0")
            self.assertEqual(command.call_args.args[0][3], identity)
        with mock.patch.object(harness, "call", return_value=result(stdout=b"another-owner")) as command:
            self.assertFalse(harness.cleanup())
            self.assertFalse(any(call.args[0][0] == "rm" for call in command.call_args_list))
        harness.network_id = "c" * 64
        def fake(arguments, **kwargs):
            self.assertTrue(kwargs["cleanup"])
            if arguments[0] == "inspect":
                raise subprocess.TimeoutExpired("fixture", 2)
            return result(stdout=harness.owner.encode())
        with mock.patch.object(harness, "call", side_effect=fake) as command:
            self.assertFalse(harness.cleanup())
            self.assertTrue(any(call.args[0] == ["network", "rm", harness.network_id] for call in command.call_args_list))
            self.assertFalse(any(call.args[0][:2] == ["volume", "rm"] for call in command.call_args_list))

    def test_racing_foreign_volume_owner_prevents_initializer_or_mount(self):
        def fake(harness, arguments, **kwargs):
            if arguments[:2] == ["image", "inspect"]:
                return result(stdout=("sha256:" + "a" * 64 + " linux/amd64").encode())
            if arguments[:2] == ["network", "create"]:
                return result(stdout=b"b" * 64)
            if arguments[:2] == ["volume", "inspect"]:
                return result(stdout=b"another-run-owner")
            return result()
        with tempfile.TemporaryDirectory() as directory:
            arguments = ["parser-boundary-smoke.py", "--parser-image", "parser:fixture", "--app-image", "app:fixture",
                         "--output", str(pathlib.Path(directory) / "evidence")]
            previous_umask = os.umask(0o077)
            try:
                with mock.patch.object(sys, "argv", arguments), mock.patch.object(Harness, "refuse_existing"), \
                        mock.patch.object(Harness, "call", autospec=True, side_effect=fake), \
                        mock.patch.object(Harness, "cleanup", return_value=True), mock.patch.object(Harness, "start") as create:
                    with self.assertRaises(ValueError):
                        MODULE["main"]()
                    create.assert_not_called()
            finally:
                os.umask(previous_umask)

    def test_cleanup_deadline_is_independent_and_command_output_is_bounded(self):
        real_popen = subprocess.Popen
        with tempfile.TemporaryDirectory() as directory:
            harness = Harness(pathlib.Path(directory), "rust-ocr-unit-smoke")
            harness.deadline = 0
            def fake(arguments, **kwargs):
                return real_popen([sys.executable, "-c", "print('synthetic')"], **kwargs)
            with mock.patch.object(subprocess, "Popen", side_effect=fake) as process:
                with self.assertRaises(ValueError):
                    harness.call(["never-run-docker"])
                process.assert_not_called()
                self.assertEqual(harness.call(["never-run-docker"], cleanup=True).stdout, b"synthetic\n")
            def oversized(arguments, **kwargs):
                return real_popen([sys.executable, "-c", "print('x' * 70000)"], **kwargs)
            with mock.patch.object(subprocess, "Popen", side_effect=oversized):
                with self.assertRaises(ValueError):
                    harness.call(["never-run-docker"], cleanup=True)
            spawned = []
            def delayed(arguments, **kwargs):
                child = real_popen([sys.executable, "-c", "import time; time.sleep(5)"], **kwargs)
                spawned.append(child)
                return child
            with mock.patch.object(subprocess, "Popen", side_effect=delayed):
                with self.assertRaises(ValueError):
                    harness.call(["never-run-docker"], budget=.05, cleanup=True)
            self.assertIsNotNone(spawned[0].poll())
            self.assertTrue(all(path.stat().st_size <= 65536 for path in pathlib.Path(directory).glob("*.log")))

    def test_ci_invokes_boundary_after_images_with_new_evidence_path(self):
        workflow = (pathlib.Path(__file__).resolve().parent.parent / ".github/workflows/ci.yml").read_text()
        boundary = workflow.index("scripts/parser-boundary-smoke.py")
        self.assertLess(workflow.index("target: parser-runtime"), boundary)
        self.assertLess(workflow.index("scripts/monitoring-smoke.py --app-image"), boundary)
        step = workflow[workflow.rfind("      - name:", 0, boundary):workflow.find("      - uses:", boundary)]
        self.assertIn("docker image inspect", step)
        self.assertIn("--parser-image", step)
        self.assertIn("--app-image", step)
        self.assertIn("runner.temp", step)

    def test_platform_pair_is_local_linux_and_matches(self):
        harness = Harness(pathlib.Path("/nonexistent"), "rust-ocr-unit-smoke")
        for platforms, valid in [(["linux/arm64", "linux/arm64"], True), (["linux/amd64", "linux/amd64"], True),
                                 (["linux/arm64", "linux/amd64"], False), (["darwin/arm64", "darwin/arm64"], False)]:
            outputs = [result(stdout=("sha256:" + "a" * 64 + " " + platform).encode()) for platform in platforms]
            with mock.patch.object(harness, "call", side_effect=outputs):
                if valid:
                    self.assertEqual(MODULE["resolve_images"](harness, ["parser", "app"])[0][1], platforms[0])
                else:
                    with self.assertRaises(ValueError):
                        MODULE["resolve_images"](harness, ["parser", "app"])


if __name__ == "__main__":
    unittest.main()
