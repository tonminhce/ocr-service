CREATE TABLE jobs (
    id UUID PRIMARY KEY,
    filename TEXT NOT NULL,
    media_type TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    input BYTEA NOT NULL CHECK (octet_length(input) <= 52428800),
    idempotency_key TEXT UNIQUE,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'processing', 'succeeded', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_token UUID,
    lease_expires_at TIMESTAMPTZ,
    result JSONB,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    CHECK ((status = 'processing') = (lease_token IS NOT NULL)),
    CHECK (status <> 'succeeded' OR result IS NOT NULL)
);

CREATE INDEX jobs_queued_due_idx ON jobs (available_at, created_at, id)
    WHERE status = 'queued';
CREATE INDEX jobs_processing_expiry_idx ON jobs (lease_expires_at, id)
    WHERE status = 'processing';
CREATE INDEX jobs_terminal_retention_idx ON jobs (updated_at)
    WHERE status IN ('succeeded', 'failed');

