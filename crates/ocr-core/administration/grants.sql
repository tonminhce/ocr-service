-- Reconcile only the fixed, application-owned objects. Do not grant privileges
-- to every future table/function or adopt tables owned by an existing admin.
REVOKE ALL ON public.jobs, public.job_outbox, public.job_review_events,
    public.region_checkpoints, public._sqlx_migrations,
    public.job_retention_holds, public.job_retention_events
    FROM PUBLIC, ocr_api, ocr_worker, ocr_maintenance, ocr_backup;
-- Table REVOKE does not remove column ACLs; explicitly reconcile these too.
REVOKE ALL (id,filename,media_type,sha256,input,idempotency_key,status,attempts,
    available_at,lease_token,lease_expires_at,result,error,created_at,updated_at,
    extraction_profile,retention_policy_id,retention_class,retain_until,
    retention_review_revision,retention_revision,retention_active_holds) ON public.jobs
    FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (job_id,next_dispatch_at,dispatch_token,dispatch_lease_expires_at)
    ON public.job_outbox FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (job_id,revision,request_id,principal,action,reason,request,snapshot,status,created_at)
    ON public.job_review_events FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (job_id,cache_key,content,content_sha256,created_at)
    ON public.region_checkpoints FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (version,description,installed_on,success,checksum,execution_time)
    ON public._sqlx_migrations FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (job_id,hold_id,created_at) ON public.job_retention_holds
    FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL (job_id,revision,request_id,expected_revision,hold_id,action,reason,
    principal,active_holds,source_present,created_at) ON public.job_retention_events
    FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
REVOKE ALL ON FUNCTION public.maintain_job_outbox(), public.protect_job_review_event()
    FROM PUBLIC, ocr_api, ocr_worker, ocr_maintenance, ocr_backup;
REVOKE ALL ON FUNCTION public.guard_job_retention_delete(),public.guard_held_job_source(),
    public.protect_job_retention_event(),
    public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)
    FROM PUBLIC,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
ALTER FUNCTION public.maintain_job_outbox() SECURITY INVOKER;
ALTER FUNCTION public.protect_job_review_event() SECURITY INVOKER;
ALTER FUNCTION public.maintain_job_outbox() SET search_path = pg_catalog, public;
ALTER FUNCTION public.protect_job_review_event() SET search_path = pg_catalog, public;

GRANT SELECT ON public._sqlx_migrations TO ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
GRANT SELECT ON public.jobs TO ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
GRANT INSERT (id,filename,media_type,sha256,input,idempotency_key,extraction_profile)
    ON public.jobs TO ocr_api;
-- SELECT FOR UPDATE in review needs UPDATE on at least one column. This grants
-- timestamp modification only, never result/source/status/identity mutation.
GRANT UPDATE (updated_at) ON public.jobs TO ocr_api;
GRANT UPDATE (status,attempts,available_at,lease_token,lease_expires_at,result,
    error,input,updated_at) ON public.jobs TO ocr_worker;

-- maintain_job_outbox is SECURITY INVOKER: queued API INSERT must be able to
-- upsert its notification, and worker terminal/retry transitions need DELETE.
GRANT SELECT, INSERT ON public.job_outbox TO ocr_api;
GRANT UPDATE (next_dispatch_at,dispatch_token,dispatch_lease_expires_at)
    ON public.job_outbox TO ocr_api;
GRANT SELECT,INSERT,UPDATE,DELETE ON public.job_outbox TO ocr_worker;
GRANT SELECT ON public.job_outbox TO ocr_maintenance,ocr_backup;
GRANT SELECT,INSERT ON public.job_review_events TO ocr_api;
GRANT SELECT ON public.job_review_events TO ocr_maintenance,ocr_backup;
GRANT SELECT,INSERT ON public.region_checkpoints TO ocr_worker;
GRANT SELECT ON public.region_checkpoints TO ocr_maintenance,ocr_backup;
-- Retention state has no direct runtime or maintenance write path. Worker uses
-- the job's guarded counter; only the owner function changes counter+state+audit
-- together. The principal is session_user, never a caller-supplied identity.
GRANT SELECT ON public.job_retention_holds,public.job_retention_events
    TO ocr_maintenance,ocr_backup;
GRANT EXECUTE ON FUNCTION public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)
    TO ocr_maintenance;
-- Trigger execution is checked when a trigger is created; direct invocation of
-- a trigger-returning function is not supported. Keep explicit function access
-- limited to the invokers instead of installing SECURITY DEFINER helpers.
GRANT EXECUTE ON FUNCTION public.maintain_job_outbox() TO ocr_api,ocr_worker;
GRANT EXECUTE ON FUNCTION public.protect_job_review_event() TO ocr_api;
-- No runtime/maintenance jobs DELETE or TRUNCATE. A04 installs DB guards and
-- hold tooling only; destructive apply remains unavailable to every caller.
