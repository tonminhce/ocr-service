-- Human decisions are separate from immutable OCR output. Retention removes both.
CREATE TABLE job_review_events (
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    request_id UUID NOT NULL,
    principal TEXT NOT NULL CHECK (octet_length(principal) BETWEEN 1 AND 128),
    action TEXT NOT NULL CHECK (action IN ('correct','approve','reject','reopen')),
    reason TEXT NOT NULL CHECK (octet_length(reason) BETWEEN 1 AND 2000),
    request JSONB NOT NULL CHECK (octet_length(request::text) <= 65536),
    snapshot JSONB NOT NULL CHECK (octet_length(snapshot::text) <= 1048576),
    status TEXT NOT NULL CHECK (status IN ('needs_review','approved','rejected')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (job_id, revision),
    UNIQUE (job_id, request_id)
);
CREATE FUNCTION protect_job_review_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' OR EXISTS (SELECT 1 FROM jobs WHERE id = OLD.job_id) THEN
        RAISE EXCEPTION 'review events are append-only';
    END IF;
    RETURN OLD;
END;
$$;
CREATE TRIGGER job_review_append_only BEFORE UPDATE OR DELETE ON job_review_events
FOR EACH ROW EXECUTE FUNCTION protect_job_review_event();
