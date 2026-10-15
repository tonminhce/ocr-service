-- Redis contains delivery notifications; PostgreSQL retains the recovery record
-- for every nonterminal job, including work whose worker lease is still live.
CREATE TABLE job_outbox (
    job_id UUID PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    next_dispatch_at TIMESTAMPTZ NOT NULL,
    dispatch_token UUID,
    dispatch_lease_expires_at TIMESTAMPTZ,
    CHECK ((dispatch_token IS NULL) = (dispatch_lease_expires_at IS NULL))
);

CREATE INDEX job_outbox_dispatch_due_idx ON job_outbox (next_dispatch_at, job_id);

CREATE FUNCTION maintain_job_outbox() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.status IN ('succeeded', 'failed') THEN
        DELETE FROM job_outbox WHERE job_id = NEW.id;
    ELSIF TG_OP = 'INSERT' OR NEW.status = 'queued' THEN
        INSERT INTO job_outbox (job_id, next_dispatch_at)
        VALUES (
            NEW.id,
            CASE WHEN NEW.status = 'processing' THEN NEW.lease_expires_at
                 ELSE NEW.available_at END
        )
        ON CONFLICT (job_id) DO UPDATE
            SET next_dispatch_at = EXCLUDED.next_dispatch_at,
                dispatch_token = NULL,
                dispatch_lease_expires_at = NULL;
    END IF;
    -- A transition to processing retains the recovery record. The dispatcher
    -- checks the current worker lease before publishing another notification.
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER jobs_maintain_outbox
AFTER INSERT OR UPDATE OF status, available_at ON jobs
FOR EACH ROW EXECUTE FUNCTION maintain_job_outbox();

-- Existing queued and in-flight work must survive the Redis cutover too.
INSERT INTO job_outbox (job_id, next_dispatch_at)
SELECT id, CASE WHEN status = 'processing' THEN lease_expires_at ELSE available_at END
FROM jobs WHERE status IN ('queued', 'processing');
