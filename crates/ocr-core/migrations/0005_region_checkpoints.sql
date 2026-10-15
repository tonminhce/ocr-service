-- Checkpoints are scoped to one uploaded job and pipeline/crop fingerprint.
-- Cascade retention with jobs; never reuse across callers/documents.
CREATE TABLE region_checkpoints (
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    cache_key TEXT NOT NULL CHECK (cache_key ~ '^[a-f0-9]{64}$'),
    content TEXT NOT NULL CHECK (octet_length(content) <= 131072),
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[a-f0-9]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (job_id, cache_key)
);
