# Round-5 implementation plan — complete ignored-suite verification

Status: plan reviewed before execution. No production-code change; this
packet closes the "ignored tests are never counted as passed" evidence
gap by running every locally-runnable ignored suite against fresh
disposable fixtures.

## Fixtures (all fresh, owner-labelled, no volumes)

1. PostgreSQL 18 container (loopback 55433, 512 MiB) as
   `TEST_DATABASE_URL` target (plain superuser-style fixture DB per the
   test guards: each test creates its own UUID schema and drops it).
2. Redis 8.2 pinned (loopback 55434) for `TEST_REDIS_URL`.
3. Redis 8.2 pinned with maxmemory (loopback 55435) for
   `TEST_REDIS_OOM_URL` (the OOM fixture sets its own maxmemory policy
   via the test; a fresh instance per the existing harness pattern).
4. `restore_target.rs` + `administration.rs` need the fixed OCR cluster
   roles: bootstrap them in the same disposable PostgreSQL using the
   canonical image's `ocr-admin roles bootstrap` + synthetic passwords
   (never printed), providing `TEST_ADMIN_DATABASE_URL`.

## Suites to run (serial, --ignored --test-threads=1)

- ocr-core: store (35), review, retention, administration, checkpoints,
  response_limits, backup_manifest, restore_target.
- ocr-api: http (ignored cases incl. bounded legacy replay/artifact/
  review contracts), transport (1 cgroup-explicit — attempt only if the
  host allows; record NOT_RUN otherwise).
- ocr-worker: worker_lifecycle (14), retry_efficiency (2), inference/
  layout cgroup cases as the host permits.

Budget: ≤90 min total; each suite ≤10 min; stop on any fixture failure
with cause recorded. Cleanup: remove the three fixture containers by
exact name; retain nothing (no volumes created).

## Acceptance

Every suite's ignored tests pass serially on the disposable fixtures, or
are recorded NOT_RUN with the concrete reason (e.g. cgroup/mount limits
on this host). Counts recorded in the trace — replacing "91 ignored,
never counted" with explicit per-suite receipts.

## Rollback

Remove fixture containers; no repository state changes beyond the trace.
