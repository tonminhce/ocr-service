# Local release candidates and rollback verification

Updated2026-10-16. O02 tooling is partially implemented. Local verification and inventories do not authorize production rollout, registry publication or live-data changes. Three successful hosted CI runs for reviewed candidate sources, trusted registry/provenance verification, deployment configuration/model approval and production acceptance remain open. The [hosted audit](o02-hosted-ci-execution-trace.md) confirms three older successes, a later failed inventory run and no run for the current local HEAD/dirty sources; the fresh-runner runtime-image prerequisite now has a locally tested workflow fix.

## What CI now checks

`.github/workflows/ci.yml` retains format, strict Clippy, ordinary and explicit serial integration tests. It also enforces source comment policy, runs the dependency audit guard, builds the Linux parser and backup verification targets, builds application/parser/backup-ops images tagged by Git commit, creates CycloneDX SBOMs and HIGH/CRITICAL reports, rejects fixable CRITICAL findings, validates the opt-in monitoring configuration/rule fixtures, and runs the packaged slow-model course-layout smoke plus isolated monitoring fault drill. Monitoring tool images are included in the configured SBOM/scan loop. It produces a local release inventory with a digest lock. These are configured jobs; editing a workflow is not evidence that hosted CI ran.

The packaged smoke uses the real API, restricted-role PostgreSQL, authenticated Redis, parser container, PDF renderer, crop/formatter/extraction and durable worker. Its `mock-layout` Rust executable produces a single explicitly synthetic text region per page; `mock-vllm` responds after12seconds. It verifies orchestration and the parser idle interval, not real PP-DocLayout/Qwen accuracy or throughput. The synthetic marker and fixture image are visible in its output/evidence. Production uses `layout-detect`, not `mock-layout`; the override is confined to `compose.production.smoke.yaml`.

The harness refuses existing Compose projects, existing output directories and external secret directories. It generates fresh synthetic secret files inside private output. Cleanup on failure stops only its own project, unless `SMOKE_KEEP=1` explicitly retains it for inspection; persistent volumes are never deleted. Metrics authentication is read from a file inside a Python HTTP client and never put in process argv. `--no-build` ensures smoke/recovery uses the requested built images.

## Dependency audit exception and guard

Run `bash scripts/audit-dependencies.sh`. An unmodified `cargo audit` reports `RUSTSEC-2023-0071` for `rsa0.9.10` in Cargo.lock. There is no patched version in the [RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html). The [SQLx upstream issue](https://github.com/launchbadge/sqlx/issues/3211) explains Cargo weak-feature resolution retaining unused database drivers in the lockfile. The current `cargo tree --workspace --locked --target all --all-features` has no active RSA/MySQL dependency.

The script first examines that complete active dependency tree and fails if RSA becomes active. Only after that guard succeeds does it apply the single advisory exception. It does not hide other vulnerabilities or informational warnings, and the original raw audit result is retained in local verification evidence. Do not add a global audit ignore or weaken the guard to accept an active RSA implementation. Reassess this exception if SQLx/features/platforms change.

Image scans initially found three fixable CRITICAL Perl advisories in the pinned Bookworm base. Runtime build stages now apply available distribution package updates before installing their tools. The base digest remains pinned; package versions are captured by SBOMs, so build-time repository changes are not described as bit-for-bit reproducible. A fixable Go standard-library advisory was also detected in unused `gosu` inherited by backup-ops. That image has an empty inherited entrypoint and runs directly as UID10001; it now removes the unused root-switch executable. PostgreSQL clients/age remain present. No advisory was hidden to pass image scanning.

The policy rejects **fixable CRITICAL** findings. Unfixed CRITICAL/HIGH findings remain visible and require review; a passing threshold is not zero vulnerabilities or proof of exploit resistance.

## Candidate inventory and digest lock

```sh
python3 scripts/release-manifest.py \
  --app-image ocr-service:COMMIT \
  --parser-image ocr-service:parser-COMMIT \
  --backup-image ocr-service:backup-COMMIT \
  --layout-model artifacts/models/pp-doclayout-v3/model.onnx \
  --monitoring \
  --output artifacts/release-COMMIT
```

Output is a new private directory containing `manifest.json`, `manifest.sha256`, `compose.lock.yaml` and `compose.backup.lock.yaml`. It records only selected Docker image identity/platform fields, exact application/build/Compose inputs and immutable migration hashes. It never reads real `.env`, secret values or expanded Compose configuration. Layout hash is measured from an optional local model file. An optional `--model-sha256` is explicitly operator-declared; absent model identity stays null. Runtime configuration attestation remains unexpanded template hashes, not proof of a running deployment's settings.

With `--monitoring`, the inventory additionally records exact local Prometheus/Alertmanager image identities from their digest-pinned Compose references, sets `monitoring_included`, and adds their services to the lock. These tool images must already be locally available and share the release platform. Omit the flag for a release without monitoring; hashing its template alone does not mean the monitoring images were inventoried. No receiver credentials or expanded runtime configuration enter the inventory.

The application lock covers production services including parser socket initialization; backup services have a separate lock so adding the production lock cannot accidentally create an extra runnable backup/mock service. All references use a repository digest when available; otherwise the inventory explicitly identifies the local engine identity. `pull_policy: never` keeps the lock local to the engine with those artifacts loaded. No mutable tag silently substitutes for a missing digest.

Use `config -q` with non-secret example settings to check merges. Do not print an expanded real deployment configuration. A deployment command must also use `--no-build`. This tooling creates no containers and performs no deployment, image push or registry authentication.

Image identity is not source provenance: local inventories label image-to-source binding operator-declared. Hosted builds using `load: true` cannot automatically be described as carrying signed registry attestations. [Docker's attestation documentation](https://docs.docker.com/build/ci/github-actions/attestations/) describes the exporter limitations. Production requires reviewed registry digests, verified provenance/SBOM binding, trusted keys/identity and a concrete immutable configuration before rollout.

## Local N → N+1 → N drill

First retain a successful disposable synthetic smoke project with `SMOKE_KEEP=1`. Then:

```sh
python3 scripts/rollback-smoke.py \
  --project rust-ocr-example-smoke-UNIQUE \
  --secrets-dir artifacts/SMOKE/secrets \
  --app-n ocr-service:PREVIOUS \
  --parser-n ocr-service:parser-PREVIOUS \
  --app-next ocr-service:CANDIDATE \
  --parser-next ocr-service:parser-CANDIDATE \
  --output artifacts/rollback-UNIQUE
```

Use the same loopback port variables as the original smoke if overriding defaults. The script requires an existing project containing only completed jobs with the known synthetic two-page PDF hash, and secret paths under local ignored artifacts. It refuses an existing evidence directory. It resolves local image IDs, stops only the test app/parser/model services, deploys each pinned pair without building, checks exact deployed identities, compares complete original job-row hashes, and runs a fresh synthetic PDF with the extraction profile on attempt1 at each stage. PostgreSQL/Redis volumes remain in place. The report never prints source values or credentials.

The drill explicitly uses legacy `full_page_vlm` to test stored data/API/parser compatibility across prior images, which do not contain the newer course detector fixture. The separate course-layout smoke verifies crop/order/extraction with slow model responses. Both are synthetic. This is a same-schema/protocol-v2 rollback drill, not proof of backwards compatibility across future migrations, real OCR quality or live backup restoration. Keep the project for evidence capture, then stop that exact project without `-v`.

## Local monitoring candidate drill

The [alert runbook](alert-runbook.md) explains safe activation on an approved existing project and the local-only receiver. The [metrics contract](observability.md) distinguishes independent bounded parser/catalog probes, pool occupancy, durable freshness and actual Redis maxmemory from model readiness and host capacity.

```sh
bash scripts/check-monitoring.sh
python3 scripts/monitoring-smoke.py \
  --app-image ocr-service:CANDIDATE \
  --parser-image ocr-service:parser-CANDIDATE \
  --output artifacts/monitoring-UNIQUE
```

The validators use pinned tools and a temporary synthetic credential. The drill requires matching already-built application/parser images, refuses an existing output/project namespace, creates fresh synthetic credentials and uses disposable services with loopback ports. It verifies local authenticated scrapes, rule firing/Alertmanager ingestion during parser/model, PostgreSQL and worker faults, recovery and preserved synthetic job rows, then scoped cleanup with volumes retained. Forty scrape requests with at most four concurrent clients form a small functional probe, not throughput sizing. It calls no real model and sends no external notification.

Both local drills passed. The strengthened repeat passed all 18 checks, including resource/secret-mount boundaries, exact original-row preservation and runtime progression during PostgreSQL outage. It ended with four succeeded synthetic jobs and five attempts; the fresh recovery job succeeded on attempt one. See the [execution trace](o01-alerting-execution-trace.md). Configured CI is not evidence of hosted execution. Keep reports/logs private under ignored artifacts; inspect current execution evidence before release acceptance.

The subsequent [parser boundary matrix](a06b-boundary-execution-trace.md) adds active IPv4 TCP denial with live positive controls, synthetic sibling mount absence, scratch noexec and worker socket permissions. It passed seven checks on specified historical Linux ARM64 images, preserved all baseline container IDs and retained its fresh socket volume. The harness accepts only locally available matching Linux image platforms and resolves immutable IDs without pulling/building; it does not start the application, database, worker or real models. CI now invokes it after building its images and running the packaged drill, and uploads its report. That is configured current-source coverage; no hosted run or current-worktree binary attestation is claimed by the historical local matrix.

## Remaining release gates

- Three consecutive hosted CI successes have not been obtained; no push/remote publication is authorized by local verification.
- No registry release or signed provenance is claimed by local image IDs/SBOMs.
- Production runtime/model/configuration approvals and a concrete migration/rollback compatibility matrix remain required.
- Monitoring thresholds, host/swap/free-disk/backup success exporters, external notification/watchdog delivery and production resource/replica discovery acceptance remain open.
- Existing live containers remain untouched. Staging/24h observation, capacity, independent labels, reviewer identity, backup destination/RPO/RTO and retention policy retain their gates.
