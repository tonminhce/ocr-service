# Redis Streams + PostgreSQL

The course separates the HTTP producer from a Redis-driven consumer. This
implementation keeps that boundary and adds a durable database. Redis carries
only a `job_id`; PostgreSQL stores the original upload, idempotency key, job
state, retry time, ownership lease, OCR result and recurring delivery outbox.

PostgreSQL-only scheduling was a deliberate valid starting point: storing the
job and its queue state in one transaction avoids a cross-system publication
gap. Redis is useful as a distribution layer, but adds another failure boundary.
The user's requested architecture now uses both systems, with explicit recovery
instead of assuming every database write and queue command succeed together.

## Submission and delivery

1. Axum inserts the job in PostgreSQL. A database trigger creates its outbox row
   in the same transaction. The API can accept work while Redis is unavailable;
   bounded admission still limits the total pending job count.
2. An independent Rust dispatcher leases a bounded outbox batch with
   `FOR UPDATE SKIP LOCKED`. It publishes `XADD stream * job_id UUID`, then marks
   that delivery with a token-fenced database write. It keeps running when all
   inference slots are occupied.
3. Rust consumers use a dedicated consumer group and process-specific UUID.
   `XREADGROUP` receives notifications; `XAUTOCLAIM` scans old pending messages.
   The consumer must claim the corresponding PostgreSQL job by ID before doing
   OCR. A duplicate notification cannot consume an extra attempt while another
   owner has a live lease.
4. The worker renews its PostgreSQL lease during rendering/inference. It commits
   a fenced success or failure in PostgreSQL before acknowledging the stream
   entry. A Lua command performs `XACK` and `XDEL` atomically.
5. A retry updates the job's durable `available_at` and resets its outbox row in
   the same transaction. Terminal success/failure removes the outbox row. Failed
   jobs remain queryable through the existing job API until retention pruning.

The outbox is recurring: after delivery, eligible jobs can be notified again
after `REDIS_REDISPATCH_SECONDS` (default 30). Active processing leases suppress
republication. This also repairs loss of an entire Redis stream, even when the
previous publish had already been recorded as successful. A failed dispatcher
lease expires; it does not permanently hide the job.

## Failure behavior

| Failure | Recovery |
| --- | --- |
| API commits the job, then stops before publishing | The transactional outbox remains for any dispatcher |
| `XADD` succeeds, but the response or outbox mark is lost | The dispatcher may publish again; conditional SQL claims reject duplicates |
| Consumer stops during OCR | SQL ownership expires; pending recovery or recurring publication permits a new attempt |
| Result commits, then ACK fails | Redelivery observes terminal database state and cleans up the notification |
| Redis is unavailable or has lost its stream/group | Bounded commands and backoff; DB jobs remain queued; group recreation plus outbox replay restore delivery |
| Inference slots are full | The independent dispatcher still publishes; consumers read only available capacity |
| Attempt budget is exhausted | PostgreSQL terminalizes the job and removes its outbox record |

Delivery and inference are at least once. Lease tokens protect the stored
result; they do not prevent repeated model execution after a crash. This is not
an exactly-once inference claim. There is no automatic switch to direct database
queue consumption when Redis fails.

## Redis operation

Compose pins Redis 8.2 Alpine by digest, uses a named `/data` volume, enables AOF
with `appendfsync everysec`, and configures `maxmemory-policy noeviction`. The
development port binds to loopback; the container uses password authentication.
Set a random hex `REDIS_PASSWORD`, with matching host `REDIS_URL` and Docker
`COMPOSE_REDIS_URL`. The example environment expands the password into both URLs.
For a separately hosted Redis service, configure its own ACL/TLS deployment and
use the matching `rediss://` URL. Do not print credential-bearing URLs in logs.

Redis AOF can lose recent writes depending on fsync policy. The recurring
PostgreSQL outbox provides the recovery record for OCR jobs independently of that
window. Database loss remains a database backup/restore concern. Redis and
PostgreSQL volumes need operational capacity monitoring; persistence alone is
not a backup.

Use **one dedicated consumer group per OCR stream**: acknowledged entries are
deleted. Additional independent groups would lose entries when the OCR group
deletes them. There is no arbitrary `MAXLEN` trimming of pending work. Consumer
metadata from crashed process UUIDs may need periodic maintenance at large
restart counts; never delete live pending work as a general cleanup strategy.

Defaults: `REDIS_STREAM=ocr:jobs`, `REDIS_CONSUMER_GROUP=ocr-workers`, command
timeout 2 seconds, recurring publication 30 seconds. Runtime errors redact Redis
URLs and credentials. API readiness checks PostgreSQL; it does not promise an
available queue, a live worker or healthy inference. Monitor oldest queued age,
outbox due age, pending stream count, failed jobs, expired leases and database
size alongside the model service.

The new `0002_job_outbox.sql` migration backfills queued/in-flight jobs. The
original migration is unchanged. New API/worker binaries perform read-only schema
checks; migrations run through the one-shot `ocr-admin` command. Local Compose
runs `schema migrate-dev`; the restricted deployment uses `schema migrate` and a
separate grants step. See [A03 operations](a03-operations.md).

Primary references: [XREADGROUP](https://redis.io/docs/latest/commands/xreadgroup/),
[XAUTOCLAIM](https://redis.io/docs/latest/commands/xautoclaim/),
[XACK](https://redis.io/docs/latest/commands/xack/),
[Redis persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/),
[PostgreSQL row locking](https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE).
