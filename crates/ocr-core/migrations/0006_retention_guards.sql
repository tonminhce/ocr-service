-- Retention metadata starts unassigned. No TTL/policy/deletion is activated.
ALTER TABLE jobs
    ADD COLUMN retention_policy_id TEXT,
    ADD COLUMN retention_class TEXT,
    ADD COLUMN retain_until TIMESTAMPTZ,
    ADD COLUMN retention_review_revision BIGINT,
    ADD COLUMN retention_revision BIGINT NOT NULL DEFAULT 0
        CHECK (retention_revision BETWEEN 0 AND 10000),
    ADD COLUMN retention_active_holds INTEGER NOT NULL DEFAULT 0
        CHECK (retention_active_holds BETWEEN 0 AND 128),
    ADD CONSTRAINT jobs_retention_release_capacity_check
        CHECK (retention_revision + retention_active_holds <= 10000),
    ADD CONSTRAINT jobs_retention_assignment_check CHECK (
        (retention_policy_id IS NULL AND retention_class IS NULL AND retain_until IS NULL
            AND retention_review_revision IS NULL)
        OR (retention_policy_id IS NOT NULL AND retention_class IS NOT NULL
            AND retain_until IS NOT NULL AND retention_review_revision IS NOT NULL
            AND retention_policy_id ~ '^[a-z][a-z0-9_.-]{0,63}$'
            AND retention_class ~ '^[a-z][a-z0-9_.-]{0,63}$'
            AND retention_review_revision >= 0)
    );

-- Match the bounded dry-run keyset cursor without sorting the complete jobs table.
CREATE INDEX jobs_retention_scan_idx ON jobs(created_at, id);

CREATE TABLE job_retention_holds (
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    hold_id UUID NOT NULL CHECK (hold_id <> '00000000-0000-0000-0000-000000000000'),
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (job_id, hold_id)
);

CREATE TABLE job_retention_events (
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision BETWEEN 1 AND 10000),
    request_id UUID NOT NULL CHECK (request_id <> '00000000-0000-0000-0000-000000000000'),
    expected_revision BIGINT NOT NULL CHECK (expected_revision BETWEEN 0 AND 9999),
    hold_id UUID NOT NULL CHECK (hold_id <> '00000000-0000-0000-0000-000000000000'),
    action TEXT NOT NULL CHECK (action IN ('hold','release')),
    reason TEXT NOT NULL CHECK (octet_length(reason) BETWEEN 1 AND 2000),
    principal TEXT NOT NULL CHECK (octet_length(principal) BETWEEN 1 AND 128),
    active_holds INTEGER NOT NULL CHECK (active_holds BETWEEN 0 AND 128),
    source_present BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (job_id, revision),
    UNIQUE (job_id, request_id)
);

CREATE FUNCTION guard_job_retention_delete() RETURNS trigger
LANGUAGE plpgsql SECURITY INVOKER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_apply_disabled';
END;
$$;
CREATE TRIGGER jobs_retention_delete_guard BEFORE DELETE ON jobs
FOR EACH ROW EXECUTE FUNCTION guard_job_retention_delete();
CREATE TRIGGER jobs_retention_truncate_guard BEFORE TRUNCATE ON jobs
FOR EACH STATEMENT EXECUTE FUNCTION guard_job_retention_delete();

CREATE FUNCTION guard_held_job_source() RETURNS trigger
LANGUAGE plpgsql SECURITY INVOKER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    -- OLD is the locked target row, including a committed hold that an old
    -- UPDATE waited behind. No stale statement snapshot/history scan is used.
    IF OLD.retention_active_holds > 0 AND NEW.input IS DISTINCT FROM OLD.input THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_held_source_immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER jobs_held_source_guard BEFORE UPDATE OF input ON jobs
FOR EACH ROW EXECUTE FUNCTION guard_held_job_source();

CREATE FUNCTION protect_job_retention_event() RETURNS trigger
LANGUAGE plpgsql SECURITY INVOKER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_events_append_only';
END;
$$;
CREATE TRIGGER job_retention_events_append_only BEFORE UPDATE OR DELETE ON job_retention_events
FOR EACH ROW EXECUTE FUNCTION protect_job_retention_event();
CREATE TRIGGER job_retention_events_no_truncate BEFORE TRUNCATE ON job_retention_events
FOR EACH STATEMENT EXECUTE FUNCTION protect_job_retention_event();

-- Render the actual migration schema so isolated-schema tests exercise the
-- same fully qualified body as production public. pg_temp is explicitly last.
DO $migration$
DECLARE
    target_schema TEXT := current_schema();
BEGIN
    EXECUTE format($definition$
CREATE FUNCTION %1$I.change_job_retention_hold(
    p_job_id UUID, p_request_id UUID, p_expected_revision BIGINT,
    p_hold_id UUID, p_action TEXT, p_reason TEXT
) RETURNS TABLE (
    revision BIGINT, active_holds INTEGER, source_present BOOLEAN,
    replayed BOOLEAN, created_at TIMESTAMPTZ
)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, %1$I, pg_temp
SET lock_timeout = '2s'
AS $body$
DECLARE
    job_revision BIGINT;
    hold_count INTEGER;
    actual_count INTEGER;
    has_source BOOLEAN;
    previous_event RECORD;
    recorded_at TIMESTAMPTZ;
BEGIN
    IF p_job_id IS NULL OR p_request_id IS NULL OR p_expected_revision IS NULL
        OR p_hold_id IS NULL OR p_action IS NULL OR p_reason IS NULL
        OR p_job_id='00000000-0000-0000-0000-000000000000'::uuid
        OR p_request_id='00000000-0000-0000-0000-000000000000'::uuid
        OR p_hold_id='00000000-0000-0000-0000-000000000000'::uuid
        OR p_expected_revision NOT BETWEEN 0 AND 10000
        OR p_action NOT IN ('hold','release')
        OR octet_length(p_reason) NOT BETWEEN 1 AND 2000
        OR p_reason !~ '[^[:space:]]'
        OR translate(p_reason,E'\n\t','') ~ '[[:cntrl:]]' THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_invalid_request';
    END IF;

    SELECT j.retention_revision,j.retention_active_holds,octet_length(j.input)>0
        INTO job_revision,hold_count,has_source
        FROM %1$I.jobs j WHERE j.id=p_job_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_job_not_found';
    END IF;

    SELECT e.* INTO previous_event FROM %1$I.job_retention_events e
        WHERE e.job_id=p_job_id AND e.request_id=p_request_id;
    IF FOUND THEN
        IF previous_event.expected_revision<>p_expected_revision
            OR previous_event.hold_id<>p_hold_id OR previous_event.action<>p_action
            OR previous_event.reason<>p_reason OR previous_event.principal<>session_user::text THEN
            RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_conflict';
        END IF;
        RETURN QUERY SELECT previous_event.revision,previous_event.active_holds,
            previous_event.source_present,true,previous_event.created_at;
        RETURN;
    END IF;
    IF job_revision<>p_expected_revision THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_conflict';
    END IF;
    IF job_revision>=10000 THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_event_limit';
    END IF;
    SELECT count(*)::integer INTO actual_count FROM (
        SELECT 1 FROM %1$I.job_retention_holds h WHERE h.job_id=p_job_id LIMIT 129
    ) bounded;
    IF actual_count<>hold_count OR actual_count>128 THEN
        RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_inconsistent_state';
    END IF;

    recorded_at := clock_timestamp();
    IF p_action='hold' THEN
        IF EXISTS(SELECT 1 FROM %1$I.job_retention_holds h WHERE h.job_id=p_job_id AND h.hold_id=p_hold_id) THEN
            RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_conflict';
        END IF;
        IF hold_count>=128 THEN
            RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_hold_limit';
        END IF;
        -- Reserve an event slot to release every admitted hold. Reaching the
        -- bounded history limit must never strand an existing active hold.
        IF job_revision+hold_count+2>10000 THEN
            RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_event_limit';
        END IF;
        INSERT INTO %1$I.job_retention_holds(job_id,hold_id,created_at)
            VALUES(p_job_id,p_hold_id,recorded_at);
        hold_count := hold_count+1;
    ELSE
        DELETE FROM %1$I.job_retention_holds h WHERE h.job_id=p_job_id AND h.hold_id=p_hold_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION USING ERRCODE='P0001', MESSAGE='retention_hold_not_found';
        END IF;
        hold_count := hold_count-1;
    END IF;
    UPDATE %1$I.jobs j SET retention_revision=job_revision+1,retention_active_holds=hold_count
        WHERE j.id=p_job_id;
    INSERT INTO %1$I.job_retention_events(job_id,revision,request_id,expected_revision,
        hold_id,action,reason,principal,active_holds,source_present,created_at)
        VALUES(p_job_id,job_revision+1,p_request_id,p_expected_revision,p_hold_id,p_action,
            p_reason,session_user::text,hold_count,has_source,recorded_at);
    RETURN QUERY SELECT job_revision+1,hold_count,has_source,false,recorded_at;
END;
$body$;
$definition$, target_schema);
    EXECUTE format('REVOKE ALL ON FUNCTION %I.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text) FROM PUBLIC',target_schema);
END;
$migration$;

REVOKE ALL ON FUNCTION guard_job_retention_delete(),guard_held_job_source(),protect_job_retention_event() FROM PUBLIC;
REVOKE ALL ON job_retention_holds,job_retention_events FROM PUBLIC;
