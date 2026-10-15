#!/usr/bin/env python3
"""Exercise the actual Redis startup validator in disposable pinned containers.

Only synthetic fixtures are mounted. No Compose files, app environment, live
services or private secret files are read. Redis itself is never started. The
image must already be available locally; Docker may not pull during this test.
Output contains only fixed case names, exit codes and pass/fail results.
"""

import json
from pathlib import Path
import subprocess
import tempfile
import uuid


IMAGE = (
    "redis:8.2-alpine@sha256:"
    "b51665e66f00759be7c3152ad5ac3c66fb2f619c13ef62dea7cc1f9914524635"
)
ERRORS = {
    b"Redis secret must contain exactly one 64-character hex password\n",
    b"Redis secret contains unsupported bytes\n",
    b"Redis secret must be 64 lowercase hex characters\n",
}


def main() -> int:
    startup = Path(__file__).resolve().parents[1] / "ops" / "redis-start.sh"
    cases = [
        ("valid_exact64", b"0" * 64, True),
        ("valid_exact64_lf", b"0" * 64 + b"\n", True),
        ("empty", b"", False),
        ("newline_only", b"\n", False),
        ("terminal_nul", b"0" * 64 + b"\0", False),
        ("embedded_nul", b"0" * 32 + b"\0" + b"0" * 32, False),
        ("embedded_nul_total64", b"0" * 31 + b"\0" + b"0" * 32, False),
        ("terminal_cr", b"0" * 64 + b"\r", False),
        ("embedded_cr", b"0" * 32 + b"\r" + b"0" * 32, False),
        ("crlf_not_generated_format", b"0" * 64 + b"\r\n", False),
        ("extra_line", b"0" * 64 + b"\n\n", False),
        ("two_short_lines", b"0" * 32 + b"\n" + b"0" * 32, False),
        ("invalid_hex", b"g" * 64, False),
        ("uppercase_hex", b"A" * 64, False),
        ("embedded_space", b"0" * 31 + b" " + b"0" * 32, False),
    ]
    results = []
    with tempfile.TemporaryDirectory(prefix="ocr-redis-validator-") as directory:
        for name, content, accepted in cases:
            # Separate files avoid reusing an active/shared bind-mount inode
            # while Docker Desktop propagates host-side writes.
            fixture = Path(directory) / name
            # Parent0700 protects host access; selective mount is readable by
            # the same non-root UID used by the Rust services.
            fixture.write_bytes(content)
            fixture.chmod(0o444)
            container = f"ocr-redis-validation-{uuid.uuid4().hex}"
            command = [
                "docker", "run", "--rm", "--pull=never", "--network", "none",
                "--name", container,
                "--user", "10001:10001", "--read-only", "--cap-drop=ALL",
                "--security-opt=no-new-privileges:true", "--entrypoint", "/bin/sh",
                "--mount", f"type=bind,source={startup},target=/opt/ocr/redis-start.sh,readonly",
                "--mount", f"type=bind,source={fixture},target=/run/secrets/redis_password,readonly",
                IMAGE, "/opt/ocr/redis-start.sh", "--validate-only",
            ]
            try:
                output = subprocess.run(command, capture_output=True, timeout=20, check=False)
                passed = output.stdout == b"" and (
                    (accepted and output.returncode == 0 and output.stderr == b"")
                    or (not accepted and output.returncode == 1 and output.stderr in ERRORS)
                )
                result = {
                    "case": name,
                    "expected": "accept" if accepted else "reject",
                    "exit_code": output.returncode,
                    "passed": passed,
                }
            except (OSError, subprocess.TimeoutExpired):
                # Never dump Docker diagnostics, mount paths or fixture bytes.
                result = {"case": name, "expected": "accept" if accepted else "reject", "exit_code": None, "passed": False}
            finally:
                try:
                    subprocess.run(
                        ["docker", "rm", "-f", container],
                        capture_output=True, timeout=5, check=False,
                    )
                except (OSError, subprocess.TimeoutExpired):
                    # A daemon outage cannot be repaired here. Never report a
                    # successful test when cleanup could not be confirmed.
                    result = {"case": name, "expected": "accept" if accepted else "reject", "exit_code": None, "passed": False}
            results.append(result)
    report = {
        "suite": "redis-secret-validator",
        "image": IMAGE,
        "persistent_redis_started": False,
        "cases": results,
        "passed": sum(item["passed"] for item in results),
        "total": len(results),
    }
    print(json.dumps(report, indent=2))
    return 0 if report["passed"] == report["total"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
