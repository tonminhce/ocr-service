# A04 — Retention preview and holds

Status: implemented and verified on disposable databases; see the
[A04 execution trace](a04-execution-trace.md) and
[verification record](research/a04-verification-2026-10-15.json). No live
migration, destructive retention or policy assignment was performed. The
production default image tag is `ocr-service:a05`; use an
explicit immutable image identity for release. The current live development
deployment remains M1; A04/A05 are verified on isolated databases.

## Deployment boundary

The [A03 operations guide](a03-operations.md) describes a new isolated deployment:
bootstrap → migrate → grant → runtime. Migration `0006_retention_guards.sql` is
additive; published migrations0001–0005 are unchanged. Current binaries require
exact schema history through0006. Old schema-five binaries fail startup against
schema six. Do not roll them back over it; release/restore rehearsal remains an
open gate.

The migration creates unassigned nullable policy/class/retain-until/review
metadata. `RETENTION_DAYS` does not assign a policy. It also installs jobs
DELETE/TRUNCATE guards, a held-source update guard, active hold state and
append-only hold/release events. Even privileged legacy `Store::prune()` cannot
delete a matching row with these guards installed. An administrator can still
disable/drop database guards; this is not a sandbox against the database owner.

Maintenance receives only SELECT and EXECUTE on one owner function. It cannot
directly update state/counters, alter audits, assign policies, delete or truncate
jobs. API/worker cannot call that function or write hold metadata. Existing
worker lifecycle rights remain. The function records PostgreSQL `session_user`
as a service principal, not an asserted human identity.

## Bounded preview

Use the settings file and project name belonging to your **isolated** deployment
on every command. `/dev/null` below deliberately avoids loading the development
`.env`; replace it with your separate non-secret settings file if needed.

```sh
docker compose --env-file /dev/null -f compose.production.yaml \
  run --rm --no-deps -T maintenance ocr-admin retention plan --limit 50
```

`limit` is1–100, default50. The result has `schema_version=ocr_retention_v1`,
`apply_enabled=false`, database `snapshot_at`, `page_rows`, overlapping
`page_reason_counts`, `items`, `has_more` and `next_cursor`. A following page uses
both cursor keys:

```sh
docker compose --env-file /dev/null -f compose.production.yaml \
  run --rm --no-deps -T maintenance ocr-admin retention plan --limit 50 \
  --after-created-at '2026-10-15T00:00:00Z' \
  --after-id '00000000-0000-4000-8000-000000000001'
```

Use the actual returned cursor, not the illustrative values. Each page is an
independent repeatable-read snapshot. Counts do not describe the entire DB or
a stable multi-page export. The indexed `(created_at,id)` keyset bounds page
fetches without offset scans. Snapshot/pool/lock/SQL work has a10-second total,
2-second lock and5-second statement budget.

Every item includes `apply_disabled`. Other exclusions are `nonterminal`,
`unassigned_policy`, `held`, `not_due`, and `review_revision_changed`.
`metadata_due=true` means the metadata predicates pass; it never authorizes
deletion. Captured review revision must match the latest observed review.

`source_payload_bytes` is the logical bytea length;
`result_stored_datum_bytes` is PostgreSQL `pg_column_size` with compression/TOAST
semantics. Neither is total storage nor reclaimed disk space. Checkpoint/audit
payloads and storage overhead are not included. The preview contains opaque job
IDs and operational metadata; protect its output. It does not return filenames,
document text, source bytes, audit reasons or reviewer corrections.

## Hold and release

Create a private request file with these fields; replace all UUID placeholders
with real values. Use a new random request ID for a new operation. The hold ID
identifies one independent hold and is reused when releasing that hold.

```json
{
  "job_id": "<existing-job-uuid>",
  "request_id": "<new-operation-uuid>",
  "expected_revision": 0,
  "hold_id": "<new-hold-uuid>",
  "reason": "Pending investigation"
}
```

```sh
docker compose --env-file /dev/null -f compose.production.yaml \
  run --rm --no-deps -T maintenance ocr-admin retention hold < private-hold.json
# A separate request uses the same hold_id, fresh request_id and current revision:
docker compose --env-file /dev/null -f compose.production.yaml \
  run --rm --no-deps -T maintenance ocr-admin retention release < private-release.json
```

Action is selected by the command. Unknown fields, including `actor`, `principal`
or `action`, are rejected. Input is capped at64KiB with a10-second total stdin
read deadline; reason is1–2000 UTF-8 bytes with safe newline/tab controls only.
Never place sensitive reasons in argv or logs. CLI errors redact supplied input;
normal stdout reports metadata, not the reason. Audit stores the bounded reason
and actual DB login. There is no reviewer identity-provider integration here.

The operation locks the parent job before checking replay/CAS and changing
active state, counter and audit in one transaction. Two holds require two releases;
releasing one does not release another. Max128 concurrent holds and10,000 lifetime
events/job; admitting a hold reserves capacity for future release. Exhaustion
returns an explicit error. No audit trimming or automatic hold release exists.

An identical request replay returns the **recorded mutation snapshot**, with
`replayed=true`; it is not a fresh statement of current hold count. Reusing the
request ID with changed fields or a different DB principal fails. A new request
requires the current retention revision. Obtain current counts/revision through
the bounded preview. Review and retention revisions are separate.

Cancellation/timeout can be ambiguous if COMMIT was already sent. Retry exactly
the same request ID and fingerprint to reconcile; do not assume timeout means
rollback. Tests cover pre-COMMIT/lock-wait rollback, not every commit outcome.

## Source preservation and deletion gate

Modern completion retains input if its normal policy or any active hold requires
it. Under the same locked write, an existing `source.original_retained` property
records actual stored-source presence. Raw OCR/layout/extraction remain unchanged.
The DB trigger also blocks old SQL that attempts to change held input. A hold
placed after a legacy completion already removed source returns
`source_present=false`; it cannot recover missing bytes. Hold/release do not
rewrite existing OCR results or backfill that metadata after completion.

```sh
ocr-admin retention apply
```

Always fails before resolving credentials or opening a connection. There is no
environment switch, deletion grant or runnable destructive path. Before enabling
deletion: choose explicit policies/classes/review grace semantics, verify bounded
dry-run and A05 backup/restore/reconciliation, then review a concrete release.
Restored snapshots alone cannot reveal post-backup deletions. Historical backups,
hold audit retention and off-host destination need their own policy.
