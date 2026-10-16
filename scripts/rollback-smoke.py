#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description="N -> N+1 -> N on an existing synthetic smoke project only")
    parser.add_argument("--project", required=True)
    parser.add_argument("--secrets-dir", type=pathlib.Path, required=True)
    parser.add_argument("--app-n", required=True)
    parser.add_argument("--parser-n", required=True)
    parser.add_argument("--app-next", required=True)
    parser.add_argument("--parser-next", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"rust-ocr-[a-z0-9-]*smoke[a-z0-9-]*", args.project):
        parser.error("only a disposable rust-ocr-*smoke* project is allowed")
    if not args.secrets_dir.resolve().is_relative_to((ROOT / "artifacts").resolve()):
        parser.error("use the synthetic smoke secret directory under local artifacts")
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    os.umask(0o077)
    environment = dict(os.environ, PROD_SECRETS_DIR=str(args.secrets_dir.resolve()),
                       SMOKE_MOCK_DELAY_MS="12000")
    base = ["docker", "compose", "--env-file", "/dev/null", "-p", args.project,
            "-f", "compose.production.yaml", "-f", "compose.production.smoke.yaml"]

    def run(command, *, log=None, budget=120):
        if log:
            with (args.output / log).open("wb") as output:
                result = subprocess.run(command, cwd=ROOT, env=environment, stdout=output,
                                        stderr=subprocess.STDOUT, timeout=budget)
        else:
            result = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, timeout=budget)
        if result.returncode:
            raise ValueError("rollback command failed; see private stage logs")
        return result.stdout.decode().strip() if not log else ""

    def sql(statement):
        return run(base + ["exec", "-T", "postgres", "psql", "-U", "ocr_admin", "-d", "ocr", "-Atc", statement])

    fixture = ROOT / "crates/ocr-worker/tests/fixtures/two_pages.pdf"
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
    if sql(f"SELECT count(*) BETWEEN 1 AND 100 AND bool_and(sha256='{digest}' AND status='succeeded') FROM jobs") != "t":
        raise ValueError("target must contain only completed synthetic two-page smoke jobs")
    ids = [str(uuid.UUID(value)) for value in sql("SELECT id FROM jobs ORDER BY id").splitlines()]
    predicate = "id IN (" + ",".join("'" + value + "'::uuid" for value in ids) + ")"
    query = f"SELECT encode(sha256(convert_to(jsonb_agg(to_jsonb(j) ORDER BY id)::text,'UTF8')),'hex') FROM jobs j WHERE {predicate}"
    preserved = sql(query)
    if not re.fullmatch(r"[0-9a-f]{64}", preserved):
        raise ValueError("invalid preservation digest")
    override = args.output / "rollback.override.yaml"
    override.write_text(json.dumps({"services": {"worker": {"environment": {"OCR_STRATEGY": "full_page_vlm"}}}}))
    base += ["-f", str(override.resolve())]
    stages = []
    for label, application, renderer in [
        ("n-before", args.app_n, args.parser_n),
        ("n-plus-one", args.app_next, args.parser_next),
        ("n-restored", args.app_n, args.parser_n),
    ]:
        application_id = run(["docker", "image", "inspect", "--format", "{{.Id}}", application])
        renderer_id = run(["docker", "image", "inspect", "--format", "{{.Id}}", renderer])
        if not all(re.fullmatch(r"sha256:[0-9a-f]{64}", value) for value in [application_id, renderer_id]):
            raise ValueError("invalid image identity")
        run(base + ["stop", "worker", "api", "parser", "mock-vllm"], log=label + "-stop.log")
        environment.update(PROD_IMAGE=application_id, PROD_PARSER_IMAGE=renderer_id)
        run(base + ["up", "-d", "--wait", "--no-build", "--no-deps", "api", "worker", "parser", "mock-vllm"], log=label + "-up.log")
        if sql(query) != preserved:
            raise ValueError("stored original source/result/job rows changed during rollback")
        run([
            "docker", "run", "--rm", "--network", args.project + "_default", "--user", "10001:10001", "--read-only",
            "--tmpfs", "/tmp:size=16m,mode=1777",
            "--mount", f"type=bind,src={fixture},dst=/input/two_pages.pdf,readonly",
            "--mount", f"type=bind,src={args.secrets_dir.resolve() / 'api_token'},dst=/run/secrets/api_token,readonly",
            "--env", "OCR_API_TOKEN_FILE=/run/secrets/api_token", "--env", "OCR_API_URL=http://api:8080",
            "--env", "CLIENT_TIMEOUT_SECONDS=300", "--env", "EXPECTED_PAGES=2", "--env", "EXTRACTION_PROFILE=vi_rental_contract_v1",
            "--entrypoint", "service-client", application_id, "/input/two_pages.pdf",
        ], log=label + "-result.json", budget=330)
        latest = sql("SELECT status || ' ' || attempts FROM jobs ORDER BY created_at DESC LIMIT 1")
        if latest != "succeeded 1" or sql(query) != preserved:
            raise ValueError("rollback stage did not preserve rows and complete a fresh job on attempt one")
        for service, expected in [("worker", application_id), ("api", application_id), ("parser", renderer_id)]:
            container = run(base + ["ps", "-q", service])
            actual = run(["docker", "inspect", "--format", "{{.Image}}", container])
            if actual != expected:
                raise ValueError("deployed image differs from pinned candidate")
        stages.append({"stage": label, "application_engine_id": application_id, "parser_engine_id": renderer_id,
                       "fresh_job": latest, "preserved_job_count": len(ids), "preserved_rows_sha256": preserved})
        print(f"Rollback stage {label}: original rows preserved, fresh synthetic profile job succeeded on attempt 1", flush=True)
    report = {"schema_version": "ocr-rollback-smoke/1.0.0", "scope": "local_synthetic_same_schema_protocol_v2",
              "strategy": "full_page_vlm", "real_model_called": False, "production_activation": False,
              "stages": stages, "result": "passed", "automatic_cleanup": False,
              "note": "project remains available for inspection; operator cleans only this disposable project without deleting volumes"}
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError):
        raise SystemExit("Rollback smoke failed; inspect private logs before any retry") from None
