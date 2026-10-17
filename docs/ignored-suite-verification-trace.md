# Round-5 execution trace — complete ignored-suite verification (91/91)

Date: 2026-10-18. No production-code change; this packet converts the
"91 ignored, never counted as passed" honesty gap into explicit
per-suite receipts on fresh disposable fixtures.

## Fixtures (fresh, owner-labelled `ignored-verify-A048710F`, no volumes)

- postgres:18 loopback 55433 (768 MiB) — `TEST_DATABASE_URL` and
  `TEST_ADMIN_DATABASE_URL` (each test creates/drops its own UUID schema;
  administration/restore_target bootstrap their own fixed OCR roles).
- redis:8.2-alpine digest-pinned loopback 55436 — `TEST_REDIS_URL`.
- redis:8.2-alpine digest-pinned loopback 55435 — `TEST_REDIS_OOM_URL`.
- Port note: 55434 was occupied by the preexisting
  `rust-ocr-verify-postgres-1`; 55436 chosen instead. No original
  touched.

## Receipts (all `--ignored --test-threads=1`, serial, disposable only)

| Suite | Passed / failed |
| --- | --- |
| ocr-core store (incl. H03 regression) | 34 / 0 |
| ocr-core review | 4 / 0 |
| ocr-core retention | 11 / 0 |
| ocr-core administration | 2 / 0 |
| ocr-core checkpoints | 6 / 0 |
| ocr-core response_limits | 1 / 0 |
| ocr-core backup_manifest | 5 / 0 |
| ocr-core restore_target | 3 / 0 |
| ocr-api http (ignored contracts) | 6 / 0 |
| ocr-api transport (cgroup-explicit) | 1 / 0 |
| ocr-worker worker_lifecycle | 14 / 0 |
| ocr-worker retry_efficiency | 2 / 0 |
| ocr-worker inference_contract (cgroup-explicit) | 1 / 0 |
| ocr-worker lib (cgroup-explicit) | 1 / 0 |
| **Workspace total** | **91 / 0 / 0 still-ignored** |

Final authoritative run: `cargo test --frozen -j 1 --workspace
--no-fail-fast -- --ignored --test-threads=1` with all four fixture env
vars → `91 passed, 0 failed, 0 ignored remaining`. Every ignored test in
the repository has now been executed and passed at least once on
disposable services at the current source (`145845f`+docs commits).

## Failed attempts (preserved)

1. First plain-Redis fixture collided with the preexisting verify
   stack's port 55434 — original untouched; moved to 55436.
2. Administration suite initially failed 0/2: missing
   `TEST_ADMIN_DATABASE_URL` (the suite bootstraps its own roles and
   needs a cluster-superuser URL). Fixed by exporting the same fixture
   URL as admin; both tests then passed.
3. First workspace-wide run halted at administration (exit 101) because
   the fresh shell lacked the admin var and cargo stops at the first
   failing binary; rerun with the full env + `--no-fail-fast` → 91/0.

## Cleanup / postflight

All three fixture containers removed by exact name; zero fixture volumes
existed (none created); all 13 original live containers verified intact.

## Meaning and limits

These are integration-behavior receipts on synthetic/disposable
fixtures: durable store fencing, review CAS/audit, retention guards,
restricted roles, checkpoint bounds, response limits, backup manifest
integrity, restore-target preflight, API legacy/replay/artifact/review
contracts, transport cgroup behavior, worker lifecycle/retry/inference
cgroup cases. They do NOT prove OCR quality, real-model capacity, or
production adoption — those gates remain as registered.
