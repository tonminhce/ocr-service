#!/usr/bin/env python3
import hashlib
import json
import re
import subprocess
import time

STDOUT_CAP = 1024 * 1024
STDERR_CAP = 256 * 1024
HEX64 = re.compile(rb"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")
BEARER = re.compile(rb"Bearer [A-Za-z0-9._~+/=-]{32,}")


class CaptureError(RuntimeError):
    pass


class Capture:
    def __init__(self, stdout, stderr, returncode, elapsed,
                 stdout_truncated, stderr_truncated):
        self.stdout = stdout
        self.stderr = stderr
        self.returncode = returncode
        self.elapsed = elapsed
        self.stdout_truncated = stdout_truncated
        self.stderr_truncated = stderr_truncated
        self.stdout_sha256 = hashlib.sha256(stdout).hexdigest()
        self.stderr_sha256 = hashlib.sha256(stderr).hexdigest()


def _capped(stream, cap):
    if len(stream) > cap:
        return stream[:cap], True
    return stream, False


def run_captured(command, *, cwd=None, env=None, timeout,
                 stdout_cap=STDOUT_CAP, stderr_cap=STDERR_CAP):
    started = time.monotonic()
    with subprocess.Popen(command, cwd=cwd, env=env,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
            raise
    stdout, stdout_truncated = _capped(stdout, stdout_cap)
    stderr, stderr_truncated = _capped(stderr, stderr_cap)
    return Capture(stdout, stderr, process.returncode, time.monotonic() - started,
                   stdout_truncated, stderr_truncated)


def parse_single_json(stdout):
    try:
        value = json.loads(stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CaptureError(f"stdout is not one complete JSON document: {error}") from None
    return value


def scan_secrets(stream, *, allow_hex64=False):
    findings = []
    if not allow_hex64:
        if HEX64.search(stream):
            findings.append("synthetic-64-hex-token")
    if BEARER.search(stream):
        findings.append("bearer-credential")
    if findings:
        raise CaptureError("secret-shaped content in captured stream: " + ", ".join(findings))


def preexisting_snapshot(project):
    result = subprocess.run(
        ["docker", "compose", "-p", project, "ps", "-a", "--format", "json"],
        capture_output=True, timeout=30)
    containers = [entry.get("ID") for entry in json.loads(result.stdout or b"[]")]
    volumes = subprocess.run(
        ["docker", "volume", "ls", "--filter", f"label=com.docker.compose.project={project}",
         "--format", "{{.Name}}"], capture_output=True, timeout=30)
    networks = subprocess.run(
        ["docker", "network", "ls", "--filter", f"label=com.docker.compose.project={project}",
         "--format", "{{.Name}}"], capture_output=True, timeout=30)
    return {
        "containers": sorted(container for container in containers if container),
        "volumes": sorted(volumes.stdout.decode().split()),
        "networks": sorted(networks.stdout.decode().split()),
    }


def assert_originals_unchanged(project, snapshot):
    current = preexisting_snapshot(project)
    for kind in ("containers", "volumes", "networks"):
        if current[kind] != snapshot[kind]:
            raise CaptureError(
                f"original {kind} changed during the run: {snapshot[kind]} -> {current[kind]}")
