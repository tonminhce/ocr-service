import argparse
import json
import os
import pathlib
import re
import selectors
import signal
import subprocess
import time
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent
LABEL = "ocr.parser-boundary"
OWNER = LABEL + ".owner"
IMAGE_ID = re.compile(r"sha256:[0-9a-f]{64}")
REFERENCE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/@:+-]{0,255}")
REQUIRED_CHECKS = {"parser_runtime_policy", "canary_positive_before", "network_denial", "mount_sentinels",
                   "scratch_noexec", "worker_socket_permissions", "canary_positive_after"}
SENTINELS = ["/run/secrets/boundary-sentinel", "/models/boundary-sentinel", "/input/boundary-sentinel"]
INSPECT = ('{"image":{{json .Image}},"user":{{json .Config.User}},"network":{{json .HostConfig.NetworkMode}},'
           '"readonly":{{json .HostConfig.ReadonlyRootfs}},"caps":{{json .HostConfig.CapDrop}},'
           '"security":{{json .HostConfig.SecurityOpt}},"memory":{{json .HostConfig.Memory}},"swap":{{json .HostConfig.MemorySwap}},'
           '"cpus":{{json .HostConfig.NanoCpus}},"pids":{{json .HostConfig.PidsLimit}},'
           '"init":{{json .HostConfig.Init}},"ports":{{json .HostConfig.PortBindings}},'
           '"tmpfs":{{json .HostConfig.Tmpfs}},"mounts":[{{range $index,$mount := .Mounts}}{{if $index}},{{end}}'
           '{"destination":{{json $mount.Destination}},"type":{{json $mount.Type}},"rw":{{json $mount.RW}}}{{end}}]}')


def require_policy(source):
    match = re.search(r"(?ms)^  parser:\n(.*?)(?=^  [a-z0-9_-]+:|\Z)", source)
    if match is None:
        raise ValueError("parser policy requires review")
    parser = match.group(1)
    keys = re.findall(r"^    (\S[^:\n]*):", parser, re.M)
    expected = {"image", "build", "user", "init", "network_mode", "restart", "read_only", "security_opt",
                "cap_drop", "depends_on", "volumes", "tmpfs", "environment", "cpus", "mem_limit", "pids_limit", "healthcheck"}
    required = ['user: "10002:10001"', "init: true", "network_mode: none", "read_only: true",
                'security_opt: ["no-new-privileges:true"]', 'cap_drop: ["ALL"]',
                "cpus: 2", "mem_limit: 2g", "pids_limit: 64",
                "/scratch:size=1g,mode=0700,uid=10002,gid=10002,noexec,nosuid,nodev",
                "- parser-socket-data:/run/ocr-parser"]
    volumes = re.search(r"(?ms)^    volumes:\n(.*?)(?=^    \S|\Z)", parser)
    tmpfs = re.search(r"(?ms)^    tmpfs:\n(.*?)(?=^    \S|\Z)", parser)
    environment = re.findall(r"^      ([A-Z][A-Z0-9_]*:.*)", parser, re.M)
    expected_env = {"OCR_PARSER_SOCKET_PATH: /run/ocr-parser/parser.sock", "OCR_PARSER_SCRATCH_DIR: /scratch",
                    "OCR_PARSER_MAX_SESSIONS: ${PROD_PARSER_MAX_SESSIONS:-8}", "RUST_LOG: ${PROD_RUST_LOG:-warn}"}
    lines = {line.strip() for line in parser.splitlines()}
    if (set(keys) != expected or len(keys) != len(expected) or not all(value in lines for value in required[:-2])
            or volumes is None or volumes.group(1).strip() != "- parser-socket-data:/run/ocr-parser"
            or tmpfs is None or tmpfs.group(1).strip() != "- " + required[-2]
            or set(environment) != expected_env or len(environment) != len(expected_env)):
        raise ValueError("parser policy requires review")


def classify_probe(result, expected, marker=b""):
    if result.returncode != expected or result.stdout.strip() != marker:
        raise ValueError("boundary control failed or is inconclusive")


def finish_checks(checks):
    if set(checks) != REQUIRED_CHECKS or len(checks) != len(REQUIRED_CHECKS):
        raise ValueError("boundary matrix is incomplete")


def require_owner(actual, expected):
    if actual != expected:
        raise ValueError("socket volume is not owned by this matrix")


class Harness:
    def __init__(self, output, project):
        self.output, self.project, self.network, self.volume = output, project, project + "-network", project + "-socket"
        self.deadline, self.sequence, self.cleanup_deadline = time.monotonic() + 140, 0, None
        self.containers, self.network_id, self.owner, self.container_ids = [], None, uuid.uuid4().hex, {}

    def call(self, arguments, budget=10, expected=0, cleanup=False):
        if cleanup and self.cleanup_deadline is None:
            self.cleanup_deadline = time.monotonic() + 30
        remaining = (self.cleanup_deadline if cleanup else self.deadline) - time.monotonic()
        if remaining <= 0:
            raise ValueError("boundary total deadline elapsed")
        self.sequence += 1
        expires = time.monotonic() + min(budget, remaining)
        process = subprocess.Popen(["docker", *arguments], cwd=ROOT, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        output = {"stdout": bytearray(), "stderr": bytearray()}
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, "stdout")
                selector.register(process.stderr, selectors.EVENT_READ, "stderr")
                while selector.get_map():
                    if time.monotonic() >= expires:
                        raise ValueError("boundary command deadline elapsed")
                    for key, _ in selector.select(max(0, min(.2, expires - time.monotonic()))):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        if sum(map(len, output.values())) + len(chunk) > 65536:
                            raise ValueError("boundary command output limit exceeded")
                        output[key.data].extend(chunk)
                status = process.wait(timeout=max(.001, expires - time.monotonic()))
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=2)
            process.stdout.close()
            process.stderr.close()
            if self.output.exists():
                (self.output / f"{self.sequence:03d}.log").write_bytes(bytes(output["stdout"]) + bytes(output["stderr"]))
        result = subprocess.CompletedProcess(arguments, status, bytes(output["stdout"]), bytes(output["stderr"]))
        if expected is not None and result.returncode != expected:
            raise ValueError("boundary Docker command failed; inspect private logs")
        return result

    def refuse_existing(self):
        for flags in [["ps", "-aq"], ["network", "ls", "-q"], ["volume", "ls", "-q"]]:
            for label in [LABEL + "=" + self.project, "com.docker.compose.project=" + self.project]:
                if self.call([*flags, "--filter", "label=" + label]).stdout.strip():
                    raise ValueError("boundary namespace already exists")
        for kind, name in [("network", self.network), ("volume", self.volume)]:
            found = self.call([kind, "inspect", "--format", "{{.Name}}", name], expected=None)
            if found.returncode != 1 or found.stdout.strip():
                raise ValueError("boundary resource already exists or cannot be inspected")
        names = self.call(["ps", "-a", "--format", "{{.Names}}", "--filter", "name=" + self.project]).stdout
        if names.strip():
            raise ValueError("boundary container names already exist")

    def start(self, name, image, arguments, user="10002:10001", network="none", memory="64m", cpus="0.25", pids="16", command=()):
        container = self.project + "-" + name
        identity = self.call(["create", "--pull=never", "--name", container, "--label", LABEL + "=" + self.project,
                          "--label", OWNER + "=" + self.owner,
                          "--user", user, "--network", network, "--read-only", "--init", "--restart=no",
                          "--cap-drop=ALL", "--security-opt=no-new-privileges:true", "--memory", memory,
                          "--memory-swap", "4g" if name == "parser" else memory, "--cpus", cpus,
                          "--pids-limit", pids, *arguments, image, *command]).stdout.decode().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", identity):
            raise ValueError("invalid owned container identity")
        self.containers.append(identity)
        self.container_ids[name] = identity
        return identity

    def exec(self, name, script, user="10002:10001", expected=0, marker=b""):
        result = self.call(["exec", "--user", user, self.container_ids[name],
                            "/usr/bin/timeout", "3", "/bin/bash", "--noprofile", "--norc", "-c", script], budget=5, expected=None)
        classify_probe(result, expected, marker)

    def cleanup(self):
        success = True
        for container in reversed(self.containers):
            try:
                owned = self.call(["inspect", "--format", '{{index .Config.Labels "' + OWNER + '"}}', container], budget=2, expected=None, cleanup=True)
                if owned.returncode == 0 and owned.stdout.decode().strip() == self.owner:
                    self.call(["rm", "-f", container], budget=2, cleanup=True)
                else:
                    success = False
            except (ValueError, OSError, subprocess.SubprocessError):
                success = False
        if self.network_id:
            try:
                owned = self.call(["network", "inspect", "--format", '{{index .Labels "' + OWNER + '"}}', self.network_id], budget=2, expected=None, cleanup=True)
                if owned.returncode == 0 and owned.stdout.decode().strip() == self.owner:
                    self.call(["network", "rm", self.network_id], budget=2, cleanup=True)
                else:
                    success = False
            except (ValueError, OSError, subprocess.SubprocessError):
                success = False
        return success


def verify_parser(metadata, image):
    mounts = {(entry["destination"], entry["type"], entry["rw"]) for entry in metadata["mounts"]}
    scratch = set(metadata["tmpfs"].get("/scratch", "").split(","))
    if (metadata["image"] != image or metadata["user"] != "10002:10001" or metadata["network"] != "none"
            or metadata["readonly"] is not True or metadata["caps"] != ["ALL"]
            or metadata["security"] != ["no-new-privileges:true"] or metadata["memory"] != 2 * 1024 ** 3 or metadata["swap"] != 4 * 1024 ** 3
            or metadata["cpus"] != 2_000_000_000 or metadata["pids"] != 64 or metadata["init"] is not True
            or metadata["ports"] or len(mounts) != len(metadata["mounts"])
            or not {("/run/ocr-parser", "volume", True)} <= mounts
            or not mounts <= {("/run/ocr-parser", "volume", True), ("/scratch", "tmpfs", True)}
            or set(metadata["tmpfs"]) != {"/scratch"}
            or not {"size=1g", "mode=0700", "uid=10002", "gid=10002", "noexec", "nosuid", "nodev"} <= scratch):
        raise ValueError("observed parser policy differs")


def resolve_images(harness, references):
    images = [harness.call(["image", "inspect", "--format", "{{.Id}} {{.Os}}/{{.Architecture}}", ref]).stdout.decode().strip().split() for ref in references]
    if any(len(image) != 2 or not IMAGE_ID.fullmatch(image[0]) or image[1] not in {"linux/arm64", "linux/amd64"} for image in images) or images[0][1] != images[1][1]:
        raise ValueError("matrix requires local Linux images on one platform")
    return images


def main():
    parser = argparse.ArgumentParser(description="Bounded synthetic parser container boundary matrix")
    parser.add_argument("--parser-image", required=True)
    parser.add_argument("--app-image", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--project", default="rust-ocr-parser-boundary-smoke-" + uuid.uuid4().hex[:10])
    args = parser.parse_args()
    if not re.fullmatch(r"rust-ocr-[a-z0-9-]*smoke[a-z0-9-]*", args.project):
        raise ValueError("only a disposable smoke namespace is allowed")
    if not all(REFERENCE.fullmatch(value) for value in [args.parser_image, args.app_image]):
        raise ValueError("valid local image references are required")
    require_policy((ROOT / "compose.production.yaml").read_text())
    if args.output.exists():
        raise ValueError("new private output directory is required")
    os.umask(0o077)
    harness = Harness(args.output.resolve(), args.project)
    harness.refuse_existing()
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    baseline = set(harness.call(["ps", "-q", "--no-trunc"]).stdout.split())
    report = {"schema_version": "ocr-parser-boundary-smoke/1.0.0", "result": "failed",
              "current_worktree_binary_attested": False, "scope": "supplied_historical_or_candidate_images_only",
              "model_called": False, "production_activated": False, "volumes_deleted": False,
              "project": args.project, "retained_volume": None, "images": {"parser": args.parser_image, "application": args.app_image},
              "checks": []}
    try:
        identities = resolve_images(harness, [args.parser_image, args.app_image])
        args.parser_image, args.app_image = [entry[0] for entry in identities]
        report["images"] = {"parser": args.parser_image, "application": args.app_image, "platform": identities[0][1]}
        network_id = harness.call(["network", "create", "--internal", "--label", LABEL + "=" + args.project, "--label", OWNER + "=" + harness.owner, harness.network]).stdout.decode().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", network_id):
            raise ValueError("invalid owned network identity")
        harness.network_id = network_id
        harness.call(["volume", "create", "--label", LABEL + "=" + args.project, "--label", OWNER + "=" + harness.owner, harness.volume])
        owner = harness.call(["volume", "inspect", "--format", '{{index .Labels "' + OWNER + '"}}', harness.volume]).stdout.decode().strip()
        require_owner(owner, harness.owner)
        report["retained_volume"] = {"name": harness.volume, "owner_uuid": harness.owner}
        socket_mount = ["--mount", "type=volume,src=" + harness.volume + ",dst=/run/ocr-parser"]
        initializer = harness.start("init", args.parser_image,
                      [*socket_mount, "--cap-add=CHOWN", "--entrypoint=/bin/sh"], user="0:0",
                      command=["-c", "chown 0:10001 /run/ocr-parser && chmod 0710 /run/ocr-parser && chown 10002:10001 /run/ocr-parser"])
        harness.call(["start", "-a", initializer])
        parser_container = harness.start("parser", args.parser_image,
                      [*socket_mount, "--tmpfs", "/scratch:size=1g,mode=0700,uid=10002,gid=10002,noexec,nosuid,nodev"],
                      memory="2g", cpus="2", pids="64")
        harness.call(["start", parser_container])
        harness.exec("parser", "for attempt in {1..20}; do parser-renderer --healthcheck >/dev/null 2>&1 && exit 0; sleep .1; done; exit 70")
        metadata = json.loads(harness.call(["inspect", "--format", INSPECT, parser_container]).stdout)
        verify_parser(metadata, args.parser_image)
        report["checks"].append("parser_runtime_policy")
        sentinel = args.output.resolve() / "synthetic-sentinel"
        sentinel.write_text("synthetic-boundary-sentinel\n")
        sentinel.chmod(0o444)
        mounts = [item for target in SENTINELS for item in ["--mount", f"type=bind,src={sentinel},dst={target},readonly"]]
        control = harness.start("control", args.parser_image, ["--entrypoint=/bin/sleep", *mounts], network=harness.network_id, command=["180"])
        harness.call(["start", control])
        canary = harness.start("canary", args.app_image,
                               ["--env=MOCK_BIND_ADDR=0.0.0.0:8000", "--entrypoint=mock-vllm"],
                               user="10001:10001", network=harness.network_id)
        harness.call(["start", canary])
        ip = harness.call(["inspect", "--format", '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}', canary]).stdout.decode().strip()
        if not re.fullmatch(r"[0-9]{1,3}(\.[0-9]{1,3}){3}", ip):
            raise ValueError("invalid synthetic canary address")
        positive = f"exec 3<>/dev/tcp/{ip}/8000; printf 'GET /health HTTP/1.1\\r\\nHost: fixture\\r\\nConnection: close\\r\\n\\r\\n' >&3; IFS= read -r line <&3; [[ $line == *' 200 '* ]]"
        harness.exec("control", "for attempt in {1..15}; do (" + positive + ") >/dev/null 2>&1 && exit 0; sleep .1; done; exit 70")
        report["checks"].append("canary_positive_before")
        harness.exec("parser", f"if (echo >/dev/tcp/{ip}/8000) 2>/dev/null; then exit 71; else printf connect_denied; exit 42; fi", expected=42, marker=b"connect_denied")
        report["checks"].append("network_denial")
        checks = " && ".join(f"test -r {path}" for path in SENTINELS)
        harness.exec("control", checks)
        checks = " && ".join(f"test ! -e {path}" for path in SENTINELS + ["/var/run/docker.sock", "/run/docker.sock"])
        harness.exec("parser", checks)
        report["checks"].append("mount_sentinels")
        harness.exec("parser", "test ! -w /usr/local/bin && test -w /scratch && [[ $(id -u) == 10002 ]] && [[ $(id -g) == 10001 ]] && grep -q '^CapEff:[[:space:]]*0000000000000000$' /proc/self/status && grep -q '^NoNewPrivs:[[:space:]]*1$' /proc/self/status")
        harness.exec("parser", "[[ $(stat -c '%u:%g:%a' /scratch) == 10002:10002:700 ]] && [[ $(stat -c '%u:%g:%a' /run/ocr-parser) == 10002:10001:710 ]]")
        harness.exec("parser", "[[ $(stat -c %a /tmp) == 1777 ]] && if (: >/tmp/boundary-readonly) 2>/dev/null; then rm /tmp/boundary-readonly; exit 71; fi", user="0:0")
        harness.exec("parser", "printf 'exit 0\\n' > /scratch/boundary-exec; chmod 700 /scratch/boundary-exec; /bin/bash /scratch/boundary-exec")
        harness.exec("parser", "/scratch/boundary-exec", expected=126)
        report["checks"].append("scratch_noexec")
        harness.exec("parser", "rm /scratch/boundary-exec; parser-renderer --healthcheck >/dev/null")
        harness.exec("parser", "parser-renderer --healthcheck >/dev/null && test ! -w /run/ocr-parser && test -S /run/ocr-parser/parser.sock && if (: >/run/ocr-parser/boundary-worker) 2>/dev/null; then rm /run/ocr-parser/boundary-worker; exit 71; fi", user="10001:10001")
        report["checks"].append("worker_socket_permissions")
        harness.exec("control", positive)
        report["checks"].append("canary_positive_after")
        finish_checks(report["checks"])
        report["matrix_passed"] = True
    finally:
        try:
            report["cleanup_passed"] = harness.cleanup()
            try:
                current = set(harness.call(["ps", "-q", "--no-trunc"], budget=5, cleanup=True).stdout.split())
                report["baseline_preserved"] = baseline <= current
            except (ValueError, OSError, subprocess.SubprocessError):
                report["baseline_preserved"] = False
            report["baseline_running_containers"] = len(baseline)
            report["result"] = "passed" if report.get("matrix_passed") and report["cleanup_passed"] and report["baseline_preserved"] else "failed"
        finally:
            (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    if report["result"] != "passed":
        raise ValueError("boundary cleanup or baseline preservation failed")
    print(json.dumps({"result": report["result"], "checks": len(report["checks"]), "current_worktree_binary_attested": False}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.SubprocessError):
        raise SystemExit("Parser boundary smoke failed or inconclusive; inspect private evidence") from None
