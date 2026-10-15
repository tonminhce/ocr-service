# A03 — Separate DB identities, one-shot migration and secret files

Use `compose.production.yaml` as a **standalone** Compose file on a dedicated
PostgreSQL cluster/volume. Role names are fixed cluster-wide; a database-scoped
inventory cannot audit unrelated databases or their direct grants. Its default project
is `rust-ocr-production`; its volumes and loopback API port8180 are separate from
the existing `rust-ocr` development deployment. The name describes the deployment
configuration; overall production readiness still follows the gates in the
[execution plan](production-execution-plan.md).

## Identity and startup contract

| Identity | Purpose and privileges |
|---|---|
| `ocr_admin` | Bootstrap/grants only; PostgreSQL image initializer. Mounted only into the opt-in `admin` service and PostgreSQL itself |
| `ocr_owner` | NOLOGIN object owner; owns migrations/application objects |
| `ocr_migrator` | One-shot login, may explicitly `SET ROLE ocr_owner`; never mounted in API/worker |
| `ocr_api` | Read jobs/artifacts, insert uploads, update only `updated_at` for review row locking, append review events; invoker outbox permissions |
| `ocr_worker` | Claim/renew/retry/complete, dispatch outbox, insert/read checkpoints; cannot insert review history |
| `ocr_maintenance` | Retention plan/read + narrow hold/release function; no direct state/audit writes or job DELETE/TRUNCATE |
| `ocr_backup` | SELECT on the explicit application tables/history; no broad cluster-wide predefined role |

Runtime credentials cannot create roles/tables/schemas/temp tables, disable the
audit trigger, update/delete review history, modify migration history, become
the owner, delete jobs or truncate them. API may not modify raw result/source or
job status. Worker still needs source/result lifecycle updates; this is not an
independent protection against a compromised worker fabricating OCR. Review
identity remains a service account here; human identity/workflow belongs to F01.

Outbox/audit functions stay **SECURITY INVOKER** with `search_path=pg_catalog,public`.
Column/table grants cover their real execution paths; no broad SECURITY DEFINER
bridge was added. API review `SELECT FOR UPDATE` requires UPDATE on one column,
so only `updated_at` is granted. Bootstrap/grants reconcile known ACLs and reject
unexpected role attributes/memberships/object ownership.
[PostgreSQL privileges](https://www.postgresql.org/docs/18/ddl-priv.html),
[role membership](https://www.postgresql.org/docs/18/role-membership.html).

The pre-mutation inventory refuses other user schemas, unknown public relations,
types/functions/indexes, unsupported extensions/FDWs/event triggers, changed user
trigger configuration and RLS policies. Role/database GUC overrides also fail,
except the exact global search paths managed by bootstrap. This is an inventory
and configuration gate, not complete physical-schema attestation: migration
history checksums do not hash the live definitions of every column/function.
Do not use it as an adoption tool for a shared or independently modified DB.
Default function EXECUTE is revoked globally for the owner before migration;
a schema-local revoke alone would leave the global PUBLIC default intact.
[Default privileges](https://www.postgresql.org/docs/18/sql-alterdefaultprivileges.html).

API and worker startup run an exact, read-only migration-history check: all
embedded versions/checksums must match and be successful; pending, dirty, unknown
or missing history fails before a listener/consumer starts. This deliberately
fails an old binary rolled back over an unsupported new schema. Migration
compatibility and release rehearsal remain separate O02 gates.

## New isolated deployment

The commands below create a **new** deployment. They do not adopt tables in an
existing volume or rotate passwords. Set project/secrets paths explicitly for
every invocation. Never point bootstrap at the current application database.

```sh
mkdir -p artifacts
cargo run --locked -p ocr-core --bin ocr-secrets -- init artifacts/production-secrets
```

The Rust provisioner uses OS-random UUIDv4 pairs (244 random bits), writes18 files,
refuses any existing target directory and never prints values. Parent mode0700
protects host access. Individual files are0444 to permit UID10001 to read the
selectively mounted file: local Compose file secrets do not remap file UID/mode.
Keep the parent private; never move these files into a public directory. A
partial write is reported and retained for inspection; init never overwrites it.
Files are plaintext on the host and need the operator's protected storage/backup
policy. This is file-based delivery, not a secret-management or at-rest-encryption
system. [Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/).

Replace `vllm_api_key` with the existing model-server credential through a secure
file editor. The generated value is an explicit placeholder; it does not grant
access to the current model. Set the non-secret values in a dedicated settings
file based on `.env.production.example`, including the actual layout SHA and
inference revision. Do not reuse/source `.env` containing development credentials.

Example commands with defaults below use `/dev/null` as the Compose env file so
the current development `.env` is not loaded. Replace it with the dedicated
non-secret settings file when needed:

```sh
docker compose --env-file /dev/null -f compose.production.yaml build admin
docker compose --env-file /dev/null -f compose.production.yaml up -d --wait postgres redis
docker compose --env-file /dev/null -f compose.production.yaml run --rm --no-deps admin ocr-admin roles bootstrap
docker compose --env-file /dev/null -f compose.production.yaml run --rm --no-deps migrate
docker compose --env-file /dev/null -f compose.production.yaml run --rm --no-deps admin ocr-admin roles grant
docker compose --env-file /dev/null -f compose.production.yaml run --rm --no-deps api ocr-admin schema check
docker compose --env-file /dev/null -f compose.production.yaml up -d --wait api worker
```

The order is **bootstrap → migrate → grant → runtime**. `migrate` completion by
itself does not give the runtime principals privileges. Repeated migration is a
no-op when history/checksums match; repeated bootstrap preserves existing
passwords. Reusing different password files will not rotate existing roles, and
authentication will fail rather than silently changing the DB.

`up --wait` checks API health and that the worker process is running; the worker
has no readiness healthcheck. Queue authentication failures can remain in its
retry loop, and inference credentials/model availability are checked by actual
requests. Complete a synthetic end-to-end job before declaring the pipeline
ready. The `compose.production.smoke.yaml` override verifies protocol and
permissions with a fixture model; it does not certify real inference readiness.

`admin` is an opt-in ops profile, never a default long-running service. Restrict
who can run it or read its secret mounts. DB/Redis have no published host ports
in this file. API8180 and protected metrics9191/9192 bind loopback. Initial W=1,
shared R=1, layout pool1 and worker2GiB preserve the measurement-first policy.
Additional non-secret API/worker knobs can be supplied via an explicit reviewed
Compose override; existing binary defaults include the M1 response/SQL bounds.

Redis reads its64-character lowercase-hex password from a file, writes a0400
config to tmpfs and starts without password-bearing argv/container environment.
Its healthcheck verifies the NOAUTH gate; authenticated queue IO is verified by
the worker. API/worker receive only their own DB URL and relevant service tokens;
no administration/migration/maintenance/backup credential is mounted there.

Run `python3 scripts/test_redis_secret_validation.py` for the focused Redis
secret-format checks. It uses the already available pinned Redis image and
disposable containers with synthetic files; validation exits before starting
Redis and prints only case names, exit codes and results.

## Secret resolution

Supported keys: `DATABASE_URL`, `REDIS_URL`, `OCR_API_TOKEN`, `OCR_REVIEW_TOKEN`,
`VLLM_API_KEY`, `METRICS_BEARER_TOKEN`, and bootstrap `OCR_*_PASSWORD` keys.
Every key accepts either a direct value or its `_FILE` setting, **never both**,
including a direct empty value. Required values must exist and be nonempty;
optional direct `""` means disabled for old Compose compatibility. A configured
empty file always fails. Whitespace-only VLLM keys are preserved literally;
use an absent/empty direct setting to disable, rather than spaces.

File input is bounded at65536 bytes including at most one terminal LF/CRLF,
which is stripped without trimming spaces. Invalid UTF-8, controls/NUL, extra
newlines and empty values fail. Unix opens with `O_NOFOLLOW|O_NONBLOCK`, checks
the opened FD is regular and refuses final symlinks/directories/FIFOs; symlinks
in parent directories are permitted. Error/Debug contain only the variable name
and fixed reason, never the value, filename or underlying I/O error. Non-Unix
file resolution fails explicitly. Secret files are read once at startup; a file
edit is not an automatic live rotation.

PostgreSQL URL query keys are checked against the installed SQLx0.8.6 parser's
supported keys before that parser runs: unknown keys otherwise emit warnings
containing their values. Invalid URL/options and connection failures expose only
fixed errors; SQLx statement/slow-query logging is disabled on these connections.
Standard URI aliases, percent-encoded socket hosts, supported SSL aliases and
`options[key]` remain accepted. This allowlist must be reviewed with driver upgrades.

Bootstrap suppresses client statement logging and uses bound values/static SQL
instead of client DDL containing passwords. PostgreSQL logging is restricted
transaction-locally for the credential path. Extension/proxy/audit tooling is
outside that control: do not enable parameter/password logging on this path.
Review the [PostgreSQL logging settings](https://www.postgresql.org/docs/18/runtime-config-logging.html)
and deployment log access. Application secret files still exist in process
memory; this delivery mechanism does not sandbox untrusted document parsers.

## Existing local deployment and retention

Local/demo `compose.yaml` retains its development DB identity and adds a one-shot
`schema migrate-dev` command. That command explicitly requires a superuser and
is never used by the restricted production file. Existing application data is
not adopted by changing `POSTGRES_USER`: the official image initializes roles
only for a new database directory. A reviewed adoption/rotation procedure for
an existing DB is still required; this tooling deliberately refuses it.

New workers have **no automatic pruning loop**. `RETENTION_DAYS` remains parsed
and recorded for compatibility, while `automatic_pruning=false` and
`retention_policy=awaiting_maintenance_policy` describe enforcement. Stored
sources/results continue consuming disk until explicit policy and deletion gates
pass. Old workers using restricted credentials cannot bypass this with their old
DELETE path. Current source includes the additive [A04 hold controls](retention-operations.md);
the historical A03 evidence predates them. Destructive apply, backup destination,
restore reconciliation and production retention remain gated.

Use `down` to stop this separate deployment while retaining its volumes. Avoid
volume deletion or credential rotation as part of a code upgrade. For an upgrade,
take the A05 backup/restore and O02 release gates before applying to existing data.

Execution evidence is recorded in [A03 execution trace](a03-execution-trace.md)
and the [sanitized verification record](research/a03-verification-2026-10-15.json).
