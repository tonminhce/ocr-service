#!/usr/bin/env python3

import argparse
import hashlib
import json
import pathlib
import re
import subprocess


ROOT = pathlib.Path(__file__).resolve().parent.parent
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
REFERENCE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/@:+-]{0,255}")


def command(arguments):
    result = subprocess.run(arguments, cwd=ROOT, capture_output=True, timeout=30)
    if result.returncode or len(result.stdout) > 1024 * 1024:
        raise ValueError("release metadata command failed; details withheld")
    return result.stdout.decode().strip()


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pin_image(metadata):
    image_id = metadata["engine_image_id"]
    if not DIGEST.fullmatch(image_id):
        raise ValueError("invalid engine image identity")
    repositories = metadata.get("repo_digests") or []
    if len(repositories) > 64:
        raise ValueError("too many image aliases")
    for reference in sorted(repositories):
        if REFERENCE.fullmatch(reference) and "@" in reference and DIGEST.fullmatch(reference.rsplit("@", 1)[1]):
            return reference, "repository_digest"
    return image_id, "local_engine_identity"


def inspect_image(reference):
    if not REFERENCE.fullmatch(reference):
        raise ValueError("invalid image reference")
    template = '{"engine_image_id":{{json .Id}},"repo_digests":{{json .RepoDigests}},"os":{{json .Os}},"architecture":{{json .Architecture}}}'
    metadata = json.loads(command(["docker", "image", "inspect", "--format", template, reference]))
    pinned, kind = pin_image(metadata)
    return {"requested_reference": reference, "pinned_reference": pinned, "identity_kind": kind, **metadata}


def main():
    parser = argparse.ArgumentParser(description="Write a local candidate inventory; never deploy or publish")
    parser.add_argument("--app-image", required=True)
    parser.add_argument("--parser-image", required=True)
    parser.add_argument("--backup-image", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--layout-model", type=pathlib.Path)
    parser.add_argument("--model-sha256")
    parser.add_argument("--monitoring", action="store_true")
    args = parser.parse_args()
    if args.model_sha256 and not re.fullmatch(r"[0-9a-f]{64}", args.model_sha256):
        parser.error("--model-sha256 requires lowercase SHA-256")
    images = {
        name: inspect_image(reference) for name, reference in {
            "application": args.app_image, "parser": args.parser_image, "backup": args.backup_image,
            "postgres": "postgres:18",
            "redis": "redis:8.2-alpine@sha256:b51665e66f00759be7c3152ad5ac3c66fb2f619c13ef62dea7cc1f9914524635",
        }.items()
    }
    if args.monitoring:
        references = command(["docker", "compose", "--env-file", "/dev/null", "-f", "compose.production.yaml",
                              "-f", "compose.monitoring.yaml", "--profile", "monitoring", "config", "--images"]).splitlines()
        for name in ["prometheus", "alertmanager"]:
            reference = next((reference for reference in references if reference.startswith(f"prom/{name}:")), None)
            if reference is None or "@sha256:" not in reference:
                raise ValueError("monitoring release requires the pinned Compose images")
            images[name] = inspect_image(reference)
    platforms = {(image["os"], image["architecture"]) for image in images.values()}
    if len(platforms) != 1:
        raise ValueError("local release images must share one platform")
    inputs = [ROOT / name for name in ["Cargo.toml", "Cargo.lock", "Dockerfile", ".dockerignore"]]
    for directory in ["crates", "licenses", "ops"]:
        inputs.extend(path for path in (ROOT / directory).rglob("*") if path.is_file())
    inputs.extend(ROOT.glob("compose*.yaml"))
    inputs.extend([ROOT / ".env.production.example", ROOT / "LICENSE", ROOT / "THIRD_PARTY_NOTICES.md"])
    hashes = {str(path.relative_to(ROOT)): sha256(path) for path in sorted(inputs)}
    manifest = {
        "schema_version": "ocr-local-release-inventory/1.0.0",
        "scope": "local_candidate_not_production_approval",
        "source_commit": command(["git", "rev-parse", "HEAD"]),
        "tracked_worktree_dirty": bool(command(["git", "status", "--porcelain", "--untracked-files=no"])),
        "image_source_binding": "operator_declared_not_cryptographic_attestation",
        "registry_publication_verified": False,
        "monitoring_included": args.monitoring,
        "images": images, "build_and_compose_inputs_sha256": hashes,
        "migration_sha256": {name: digest for name, digest in hashes.items() if "/migrations/" in name},
        "model": {"sha256": args.model_sha256, "attestation": "operator_declared" if args.model_sha256 else "not_provided"},
        "layout": {"sha256": sha256(args.layout_model) if args.layout_model else None, "attestation": "local_file_hash" if args.layout_model else "not_provided"},
        "runtime_configuration_attestation": "unexpanded_templates_only_no_secrets_read",
        "remaining_gates": ["registry publication/provenance verification", "three successful hosted CI runs", "rollback drill", "model/configuration approval", "quality/capacity/operations acceptance"],
    }
    mapping = {name: "application" for name in ["api", "worker", "admin", "migrate", "maintenance"]}
    mapping.update({"parser": "parser", "parser-socket-init": "parser", "postgres": "postgres", "redis": "redis"})
    if args.monitoring:
        mapping.update({"prometheus": "prometheus", "alertmanager": "alertmanager"})
    lock = {"services": {service: {"image": images[image]["pinned_reference"], "pull_policy": "never"} for service, image in mapping.items()}}
    backup_lock = {"services": {service: {"image": images["backup"]["pinned_reference"], "pull_policy": "never"} for service in ["backup", "backup-verify"]}}
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name, value in [("compose.lock.yaml", lock), ("compose.backup.lock.yaml", backup_lock), ("manifest.json", manifest)]:
        path = args.output / name
        with path.open("x") as output:
            path.chmod(0o600)
            json.dump(value, output, indent=2)
            output.write("\n")
    seal = args.output / "manifest.sha256"
    seal.write_text(sha256(args.output / "manifest.json") + "\n")
    seal.chmod(0o600)
    print("Local release inventory written; no deployment or publication performed")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError):
        raise SystemExit("Release inventory failed; no release approval implied") from None
