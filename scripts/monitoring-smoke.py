#!/usr/bin/env python3

import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import re
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import sys
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import verify_capture


def main():
    parser = argparse.ArgumentParser(description="Isolated synthetic OCR monitoring and recovery drill")
    parser.add_argument("--app-image", required=True)
    parser.add_argument("--parser-image", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    project = "rust-ocr-monitoring-smoke-" + uuid.uuid4().hex[:10]
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    (output / "logs").mkdir(mode=0o700)
    os.umask(0o077)
    ports = set()
    while len(ports) < 5:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            ports.add(listener.getsockname()[1])
    api, api_metrics, worker_metrics, prometheus, alertmanager = sorted(ports)
    environment = dict(os.environ, PROD_IMAGE=args.app_image, PROD_PARSER_IMAGE=args.parser_image,
                       SMOKE_PROJECT=project, SMOKE_OUTPUT_DIR=str(output / "smoke"), SMOKE_KEEP="1",
                       PROD_SECRETS_DIR=str(output / "smoke/secrets"),
                       PROD_API_PORT=str(api), PROD_API_METRICS_PORT=str(api_metrics),
                       PROD_WORKER_METRICS_PORT=str(worker_metrics),
                       MONITORING_PROMETHEUS_PORT=str(prometheus), MONITORING_ALERTMANAGER_PORT=str(alertmanager))
    compose = ["docker", "compose", "--env-file", "/dev/null", "-p", project,
               "-f", "compose.production.yaml", "-f", "compose.production.smoke.yaml",
               "-f", "compose.monitoring.yaml", "--profile", "monitoring"]
    report = {"schema_version": "ocr-monitoring-smoke/1.0.0", "project": project,
              "real_model_called": False, "external_notifications_sent": False,
              "checks": [], "captures": []}
    sequence = 0

    def run(command, name, timeout=120):
        nonlocal sequence
        sequence += 1
        capture = verify_capture.run_captured(command, cwd=ROOT, env=environment,
                                               timeout=timeout)
        verify_capture.scan_secrets(capture.stdout, allow_hex64=True)
        verify_capture.scan_secrets(capture.stderr, allow_hex64=True)
        with (output / "logs" / f"{sequence:02d}-{name}.log").open("wb") as log:
            log.write(capture.stdout)
        with (output / "logs" / f"{sequence:02d}-{name}.stderr").open("wb") as log:
            log.write(capture.stderr)
        report["captures"].append({
            "name": name,
            "returncode": capture.returncode,
            "elapsed_seconds": capture.elapsed,
            "stdout_sha256": capture.stdout_sha256,
            "stderr_sha256": capture.stderr_sha256,
            "stdout_truncated": capture.stdout_truncated,
            "stderr_truncated": capture.stderr_truncated,
        })
        if capture.returncode:
            raise RuntimeError(f"{name} failed; inspect its private log")

    def request(port, path, token=None):
        headers = {"Authorization": "Bearer " + token} if token else {}
        req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers=headers)
        with urllib.request.urlopen(req, timeout=5) as response:
            body = response.read(1024 * 1024 + 1)
        if len(body) > 1024 * 1024:
            raise RuntimeError("monitoring response exceeded evidence budget")
        return body

    def query(expression):
        value = json.loads(request(prometheus, "/api/v1/query?" + urllib.parse.urlencode({"query": expression})))
        if value.get("status") != "success":
            raise RuntimeError("Prometheus query did not succeed")
        return value["data"]["result"]

    def scalar(expression):
        rows = query(expression)
        return float(rows[0]["value"][1]) if len(rows) == 1 else None

    def wait_for(name, predicate, budget=180):
        deadline = time.monotonic() + budget
        while time.monotonic() < deadline:
            try:
                if predicate():
                    report["checks"].append({"name": name, "result": "passed"})
                    print(name + ": passed", flush=True)
                    return
            except (urllib.error.URLError, ValueError, KeyError, OSError):
                pass
            time.sleep(2)
        raise RuntimeError(name + " timed out")

    def active_alerts(name):
        return [alert for alert in json.loads(request(alertmanager, "/api/v2/alerts"))
                if alert["labels"].get("alertname") == name and alert["status"]["state"] == "active"]

    def metrics_map(token):
        body = request(worker_metrics, "/metrics", token).decode()
        return {line.split(" ", 1)[0]: float(line.split(" ", 1)[1]) for line in body.splitlines()
                if line and not line.startswith("#")}

    def capture(command):
        return subprocess.check_output(command, cwd=ROOT, env=environment, text=True,
                                       stderr=subprocess.PIPE, timeout=30).strip()

    def sql(statement):
        return capture(compose + ["exec", "-T", "postgres", "psql", "-X", "-v", "ON_ERROR_STOP=1",
                                  "-U", "ocr_admin", "-d", "ocr", "-Atc", statement])

    def check_monitoring_boundaries():
        template = ('{"image":{{json .Image}},"user":{{json .Config.User}},'
                    '"read_only_rootfs":{{json .HostConfig.ReadonlyRootfs}},'
                    '"cap_drop":{{json .HostConfig.CapDrop}},"security_opt":{{json .HostConfig.SecurityOpt}},'
                    '"memory":{{json .HostConfig.Memory}},"nano_cpus":{{json .HostConfig.NanoCpus}},'
                    '"pids_limit":{{json .HostConfig.PidsLimit}},"port_bindings":{{json .HostConfig.PortBindings}},'
                    '"mounts":[{{range $i,$m := .Mounts}}{{if $i}},{{end}}'
                    '{"destination":{{json $m.Destination}},"type":{{json $m.Type}},"rw":{{json $m.RW}}}'
                    '{{end}}]}')
        specifications = {
            "prometheus": {
                "reference": "prom/prometheus:v3.15.0@sha256:efd719c99d83b060d9daefdcf00360461adf279f45ef5391f8d111892118753e",
                "memory": 256 * 1024 * 1024, "nano_cpus": 500000000, "port": 9090, "host_port": prometheus,
                "mounts": {("/etc/ocr-monitoring", "bind", False), ("/prometheus", "volume", True),
                           ("/run/secrets/metrics_token", "bind", False)},
            },
            "alertmanager": {
                "reference": "prom/alertmanager:v0.34.1@sha256:e9733bafb1bdef9b00e25a21f8f99dc26a22224bf16641ad754d1649f4c3357a",
                "memory": 128 * 1024 * 1024, "nano_cpus": 250000000, "port": 9093, "host_port": alertmanager,
                "mounts": {("/etc/ocr-monitoring", "bind", False), ("/alertmanager", "volume", True)},
            },
        }
        observed = {}
        for service, expected in specifications.items():
            container = capture(compose + ["ps", "-q", service])
            if not container or "\n" in container:
                raise RuntimeError("monitoring boundary check requires exactly one " + service)
            metadata = json.loads(capture(["docker", "inspect", "--format", template, container]))
            image = capture(["docker", "image", "inspect", "--format", "{{.Id}}", expected["reference"]])
            ports = {str(expected["port"]) + "/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(expected["host_port"])}]}
            mounts = {(mount["destination"], mount["type"], mount["rw"]) for mount in metadata["mounts"]}
            if (metadata["image"] != image or metadata["user"] != "65534:65534"
                    or metadata["read_only_rootfs"] is not True or metadata["cap_drop"] != ["ALL"]
                    or metadata["security_opt"] != ["no-new-privileges:true"]
                    or metadata["memory"] != expected["memory"] or metadata["nano_cpus"] != expected["nano_cpus"]
                    or metadata["pids_limit"] != 64 or metadata["port_bindings"] != ports
                    or len(mounts) != len(metadata["mounts"]) or not expected["mounts"].issubset(mounts)
                    or not mounts.issubset(expected["mounts"] | {("/tmp", "tmpfs", True)})):
                raise RuntimeError("monitoring resource or credential boundary differs from the pinned " + service + " contract")
            observed[service] = metadata
        report["monitoring_resource_boundaries"] = observed
        report["checks"].append({"name": "monitoring_resource_and_secret_mount_boundaries", "result": "passed"})

    for category, flags in [("containers", ["ps", "-aq"]), ("volumes", ["volume", "ls", "-q"]), ("networks", ["network", "ls", "-q"])]:
        existing = subprocess.check_output(["docker", *flags, "--filter", "label=com.docker.compose.project=" + project], text=True)
        if existing.strip():
            raise RuntimeError("refusing an existing " + category + " namespace")
    try:
        run(["bash", "scripts/production-smoke.sh"], "pipeline-smoke", timeout=420)
        run(compose + ["up", "-d", "--wait", "--no-build", "--no-deps", "prometheus", "alertmanager"], "monitoring-up")
        check_monitoring_boundaries()
        source_digest = hashlib.sha256((ROOT / "crates/ocr-worker/tests/fixtures/two_pages.pdf").read_bytes()).hexdigest()
        if sql(f"SELECT count(*)=3 AND bool_and(status='succeeded' AND sha256='{source_digest}' AND encode(sha256(input),'hex')='{source_digest}') AND sum(attempts)=4 FROM jobs") != "t":
            raise RuntimeError("initial monitoring drill must contain exactly three completed synthetic fixture jobs and four attempts")
        original_ids = [str(uuid.UUID(value)) for value in sql("SELECT id FROM jobs ORDER BY id LIMIT 4").splitlines()]
        if len(original_ids) != 3:
            raise RuntimeError("unexpected initial synthetic job identity count")
        original_predicate = "id IN (" + ",".join("'" + value + "'::uuid" for value in original_ids) + ")"
        original_query = f"SELECT encode(sha256(convert_to(jsonb_agg(to_jsonb(j) ORDER BY id)::text,'UTF8')),'hex') FROM jobs j WHERE {original_predicate}"
        original_digest = sql(original_query)
        if not re.fullmatch(r"[0-9a-f]{64}", original_digest):
            raise RuntimeError("original synthetic job-row preservation digest is invalid")
        report["preserved_jobs"] = {"count": 3, "ids": original_ids, "source_sha256": source_digest,
                                    "complete_rows_sha256": original_digest}
        wait_for("prometheus_ready", lambda: request(prometheus, "/-/ready"), budget=45)
        wait_for("alertmanager_ready", lambda: request(alertmanager, "/-/ready"), budget=45)
        wait_for("four_authenticated_and_self_scrape_targets", lambda: scalar("sum(up)") == 4)
        token = (output / "smoke/secrets/metrics_token").read_text().strip()
        try:
            request(worker_metrics, "/metrics")
        except urllib.error.HTTPError as error:
            if error.code != 401:
                raise
        else:
            raise RuntimeError("worker metrics unexpectedly allowed an unauthenticated scrape")
        report["checks"].append({"name": "unauthenticated_scrape_denied", "result": "passed"})
        wait_for("runtime_and_durable_collectors_fresh", lambda: scalar('sum(ocr_dependency_up{job="ocr-worker"})') == 2
                 and scalar('ocr_operational_collector_error{job="ocr-worker"}') == 0)
        values = metrics_map(token)
        if values.get("ocr_operational_redis_maxmemory_bytes") != 134217728:
            raise RuntimeError("Redis maxmemory observation differs from the configured 128MiB")
        timings = []
        def scrape(_):
            started = time.monotonic()
            request(worker_metrics, "/metrics", token)
            return time.monotonic() - started
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as clients:
            timings = sorted(clients.map(scrape, range(40)))
        report["scrape_probe"] = {"requests": 40, "max_clients": 4, "median_seconds": timings[20],
                                  "p95_seconds": timings[37], "capacity_certification": False}

        run(compose + ["stop", "-t", "5", "parser"], "parser-stop")
        run(compose + ["pause", "mock-vllm"], "model-pause")
        wait_for("dependency_unavailability_fires_for_parser_and_model", lambda: scalar('count(ALERTS{alertname="OCRDependencyUnavailable",alertstate="firing"})') == 2)
        wait_for("alertmanager_receives_dependency_alerts", lambda: len(active_alerts("OCRDependencyUnavailable")) == 2)
        if scalar('up{job="ocr-api"}') != 1 or scalar('up{job="ocr-worker"}') != 1:
            raise RuntimeError("dependency fault must not hide intake/exporter availability")
        report["dependency_fault_alerts"] = [{"labels": alert["labels"], "status": alert["status"]["state"]}
                                              for alert in active_alerts("OCRDependencyUnavailable")]
        run(compose + ["start", "parser"], "parser-start")
        run(compose + ["unpause", "mock-vllm"], "model-unpause")
        wait_for("dependency_alerts_resolve", lambda: scalar('sum(ocr_dependency_up{job="ocr-worker"})') == 2
                 and not active_alerts("OCRDependencyUnavailable"))

        prior_sample = scalar('ocr_runtime_last_sample_timestamp_seconds{job="ocr-worker"}')
        run(compose + ["stop", "-t", "5", "postgres"], "postgres-stop")
        wait_for("durable_collector_failure_fires", lambda: scalar('count(ALERTS{alertname="OCRCollectorFailed",alertstate="firing"})') == 1)
        wait_for("alertmanager_receives_collector_failure", lambda: len(active_alerts("OCRCollectorFailed")) == 1)
        if scalar('sum(ocr_dependency_up{job="ocr-worker"})') != 2 or scalar('ocr_runtime_last_sample_timestamp_seconds{job="ocr-worker"}') <= prior_sample:
            raise RuntimeError("runtime probes stopped progressing during database outage")
        run(compose + ["start", "postgres"], "postgres-start")
        wait_for("durable_collector_recovers", lambda: scalar('ocr_operational_collector_error{job="ocr-worker"}') == 0
                 and not active_alerts("OCRCollectorFailed"))

        started = time.monotonic()
        run(compose + ["stop", "-t", "5", "worker"], "worker-stop")
        report["worker_shutdown_seconds"] = time.monotonic() - started
        container = subprocess.check_output(compose + ["ps", "-aq", "worker"], cwd=ROOT, env=environment, text=True).strip()
        exit_code = subprocess.check_output(["docker", "inspect", "--format", "{{.State.ExitCode}}", container], text=True).strip()
        if exit_code != "0":
            raise RuntimeError("worker did not stop cleanly")
        wait_for("worker_target_down_fires", lambda: scalar('count(ALERTS{alertname="OCRTargetDown",job="ocr-worker",alertstate="firing"})') == 1)
        wait_for("alertmanager_receives_worker_target_down", lambda: bool(active_alerts("OCRTargetDown")))
        run(compose + ["start", "worker"], "worker-start")
        wait_for("worker_target_and_observations_recover", lambda: scalar('up{job="ocr-worker"}') == 1
                 and scalar('sum(ocr_dependency_up{job="ocr-worker"})') == 2 and not active_alerts("OCRTargetDown"))
        if sql(original_query) != original_digest:
            raise RuntimeError("original synthetic job rows changed during monitoring faults")
        report["checks"].append({"name": "original_complete_job_rows_preserved_after_faults", "result": "passed"})
        run([
            "docker", "run", "--rm", "--network", project + "_default", "--user", "10001:10001", "--read-only",
            "--tmpfs", "/tmp:size=16m,mode=1777",
            "--mount", f"type=bind,src={ROOT / 'crates/ocr-worker/tests/fixtures/two_pages.pdf'},dst=/input/two_pages.pdf,readonly",
            "--mount", f"type=bind,src={output / 'smoke/secrets/api_token'},dst=/run/secrets/api_token,readonly",
            "--env", "OCR_API_TOKEN_FILE=/run/secrets/api_token", "--env", "OCR_API_URL=http://api:8080",
            "--env", "CLIENT_TIMEOUT_SECONDS=300", "--env", "EXPECTED_PAGES=2", "--env", "EXTRACTION_PROFILE=vi_rental_contract_v1",
            "--entrypoint", "service-client", args.app_image, "/input/two_pages.pdf",
        ], "fresh-job-after-recovery", timeout=330)
        result = json.loads((output / "logs" / f"{sequence:02d}-fresh-job-after-recovery.log").read_text())
        if result.get("strategy") != "course_layout_vlm" or not result.get("extraction"):
            raise RuntimeError("fresh post-fault job lacks course-layout extraction")
        if sql(original_query) != original_digest:
            raise RuntimeError("original synthetic job rows changed after the fresh recovery job")
        if sql(f"SELECT count(*)=4 AND bool_and(status='succeeded' AND sha256='{source_digest}' AND encode(sha256(input),'hex')='{source_digest}') AND sum(attempts)=5 FROM jobs") != "t":
            raise RuntimeError("recovered drill must contain exactly four succeeded synthetic jobs and five attempts")
        fresh_rows = sql(f"SELECT id FROM jobs WHERE NOT ({original_predicate}) AND status='succeeded' AND attempts=1").splitlines()
        if len(fresh_rows) != 1:
            raise RuntimeError("exactly one fresh job must succeed on attempt one")
        fresh_id = str(uuid.UUID(fresh_rows[0]))
        if sql("SELECT id::text || ' ' || status || ' ' || attempts FROM jobs ORDER BY created_at DESC LIMIT 1") != fresh_id + " succeeded 1":
            raise RuntimeError("latest synthetic job is not the fresh successful recovery job")
        report["recovered_jobs"] = {"succeeded": 4, "attempts": 5, "fresh_id": fresh_id,
                                     "fresh_attempts": 1, "original_complete_rows_sha256": original_digest}
        report["checks"].append({"name": "original_complete_job_rows_preserved_after_fresh_job", "result": "passed"})
        report["checks"].append({"name": "fresh_profile_job_after_all_faults", "result": "passed"})
        report["result"] = "passed"
        report["targets"] = json.loads(request(prometheus, "/api/v1/targets"))["data"]["activeTargets"]
    finally:
        try:
            run(compose + ["unpause", "mock-vllm"], "cleanup-unpause", timeout=30)
        except (RuntimeError, subprocess.SubprocessError):
            pass
        run(compose + ["down"], "cleanup-down", timeout=60)
        report["cleanup"] = "project removed; volumes retained"
        (output / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
    print("Monitoring fault drill passed; no live service or external notification changed", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error)) from None
