-- Inventory guard, evaluated BEFORE bootstrap/grant mutations. It is not full
-- physical schema attestation; supported object names, kinds, owners and links
-- are checked so an unrelated/shared database is refused instead of hardened.
WITH tables(name) AS (VALUES
    ('jobs'),('job_outbox'),('job_review_events'),('region_checkpoints'),('_sqlx_migrations'),
    ('job_retention_holds'),('job_retention_events')
), indexes(name,parent) AS (VALUES
    ('jobs_pkey','jobs'),('jobs_idempotency_key_key','jobs'),
    ('jobs_queued_due_idx','jobs'),('jobs_processing_expiry_idx','jobs'),
    ('jobs_terminal_retention_idx','jobs'),('job_outbox_pkey','job_outbox'),
    ('jobs_retention_scan_idx','jobs'),
    ('job_outbox_dispatch_due_idx','job_outbox'),
    ('job_review_events_pkey','job_review_events'),
    ('job_review_events_job_id_request_id_key','job_review_events'),
    ('region_checkpoints_pkey','region_checkpoints'),
    ('_sqlx_migrations_pkey','_sqlx_migrations'),
    ('job_retention_holds_pkey','job_retention_holds'),
    ('job_retention_events_pkey','job_retention_events'),
    ('job_retention_events_job_id_request_id_key','job_retention_events')
), allowed_tables AS (
    SELECT c.oid,c.relname,c.reltype FROM pg_class c
    JOIN pg_namespace n ON n.oid=c.relnamespace
    JOIN tables t ON t.name=c.relname
    WHERE n.nspname='public' AND c.relkind='r'
), retention_present AS (
    -- Empty bootstrap and the immutable0001–5 schema are valid upgrade inputs.
    -- Once any0006 object exists, the guard/function set must be complete.
    SELECT EXISTS(SELECT 1 FROM allowed_tables WHERE relname IN
        ('job_retention_holds','job_retention_events')) OR EXISTS(
        SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
        WHERE n.nspname='public' AND p.proname IN ('guard_job_retention_delete',
            'guard_held_job_source','protect_job_retention_event','change_job_retention_hold'))
        OR EXISTS(SELECT 1 FROM pg_attribute a JOIN allowed_tables t ON t.oid=a.attrelid
            WHERE t.relname='jobs' AND a.attname='retention_revision' AND NOT a.attisdropped)
        AS present
)
SELECT
    EXISTS(SELECT 1 FROM pg_namespace
        WHERE left(nspname,3)<>'pg_' AND nspname NOT IN ('public','information_schema'))
    OR EXISTS(SELECT 1 FROM pg_class c
        JOIN pg_namespace n ON n.oid=c.relnamespace
        JOIN pg_roles r ON r.oid=c.relowner
        WHERE n.nspname='public' AND (
            r.rolname<>'ocr_owner' OR c.relrowsecurity OR c.relforcerowsecurity OR NOT (
                (c.relkind='r' AND c.relname IN (SELECT name FROM tables)) OR
                (c.relkind='i' AND EXISTS(
                    SELECT 1 FROM indexes a JOIN pg_index i ON i.indexrelid=c.oid
                    JOIN allowed_tables t ON t.oid=i.indrelid
                    WHERE a.name=c.relname AND a.parent=t.relname))
            )
        ))
    OR EXISTS(SELECT 1 FROM pg_proc p
        JOIN pg_namespace n ON n.oid=p.pronamespace
        JOIN pg_roles r ON r.oid=p.proowner
        WHERE n.nspname='public' AND NOT coalesce((
            r.rolname='ocr_owner' AND p.prokind='f'
            AND p.prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
            AND p.provariadic=0 AND p.pronargdefaults=0 AND p.prosupport=0
            AND NOT p.proleakproof AND NOT p.proisstrict
            AND p.provolatile='v' AND p.proparallel='u' AND (
                (p.pronargs=0 AND p.proallargtypes IS NULL AND p.proargmodes IS NULL
                    AND NOT p.proretset AND NOT p.prosecdef
                    AND p.prorettype='pg_catalog.trigger'::regtype AND (
                        (p.proname IN ('maintain_job_outbox','protect_job_review_event')
                            AND (p.proconfig IS NULL OR p.proconfig=ARRAY['search_path=pg_catalog, public']))
                        OR (p.proname IN ('guard_job_retention_delete','guard_held_job_source',
                            'protect_job_retention_event')
                            AND p.proconfig=ARRAY['search_path=pg_catalog, pg_temp'])
                    ))
                OR (p.oid=to_regprocedure('public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)')
                    AND p.pronargs=6 AND p.prosecdef AND p.proretset
                    AND p.prorettype='pg_catalog.record'::regtype
                    AND p.proconfig=ARRAY['search_path=pg_catalog, public, pg_temp','lock_timeout=2s']
                    AND p.proallargtypes=ARRAY[
                        'pg_catalog.uuid'::regtype::oid,'pg_catalog.uuid'::regtype::oid,
                        'pg_catalog.int8'::regtype::oid,'pg_catalog.uuid'::regtype::oid,
                        'pg_catalog.text'::regtype::oid,'pg_catalog.text'::regtype::oid,
                        'pg_catalog.int8'::regtype::oid,'pg_catalog.int4'::regtype::oid,
                        'pg_catalog.bool'::regtype::oid,'pg_catalog.bool'::regtype::oid,
                        'pg_catalog.timestamptz'::regtype::oid]
                    AND p.proargmodes=ARRAY['i','i','i','i','i','i','t','t','t','t','t']::"char"[]
                    AND p.proargnames[7:11]=ARRAY['revision','active_holds','source_present','replayed','created_at'])
            )
        ),false))
    OR EXISTS(SELECT 1 FROM pg_type y
        JOIN pg_namespace n ON n.oid=y.typnamespace
        JOIN pg_roles r ON r.oid=y.typowner
        WHERE n.nspname='public' AND (r.rolname<>'ocr_owner' OR NOT (
            (y.typtype='c' AND EXISTS(SELECT 1 FROM allowed_tables t
                WHERE t.oid=y.typrelid AND t.reltype=y.oid AND t.relname=y.typname))
            OR (y.typtype='b' AND EXISTS(
                SELECT 1 FROM allowed_tables t JOIN pg_type base ON base.oid=t.reltype
                WHERE base.typarray=y.oid AND y.typelem=base.oid))
        )))
    OR EXISTS(SELECT 1 FROM pg_trigger g JOIN allowed_tables t ON t.oid=g.tgrelid
        WHERE NOT g.tgisinternal AND (g.tgenabled<>'O' OR g.tgqual IS NOT NULL
            OR g.tgnargs<>0 OR octet_length(g.tgargs)<>0 OR g.tgconstraint<>0
            OR g.tgparentid<>0 OR g.tgdeferrable OR g.tginitdeferred
            OR g.tgoldtable IS NOT NULL OR g.tgnewtable IS NOT NULL OR NOT coalesce((
            (t.relname='jobs' AND g.tgname='jobs_maintain_outbox'
                AND g.tgtype=21 AND g.tgattr='7 9'::int2vector
                AND g.tgfoid=to_regprocedure('public.maintain_job_outbox()'))
            OR (t.relname='job_review_events' AND g.tgname='job_review_append_only'
                AND g.tgtype=27 AND g.tgattr=''::int2vector
                AND g.tgfoid=to_regprocedure('public.protect_job_review_event()'))
            OR (t.relname='jobs' AND g.tgname='jobs_retention_delete_guard'
                AND g.tgtype=11 AND g.tgattr=''::int2vector
                AND g.tgfoid=to_regprocedure('public.guard_job_retention_delete()'))
            OR (t.relname='jobs' AND g.tgname='jobs_retention_truncate_guard'
                AND g.tgtype=34 AND g.tgattr=''::int2vector
                AND g.tgfoid=to_regprocedure('public.guard_job_retention_delete()'))
            OR (t.relname='jobs' AND g.tgname='jobs_held_source_guard'
                AND g.tgtype=19 AND g.tgattr='5'::int2vector
                AND g.tgfoid=to_regprocedure('public.guard_held_job_source()'))
            OR (t.relname='job_retention_events' AND g.tgname='job_retention_events_append_only'
                AND g.tgtype=27 AND g.tgattr=''::int2vector
                AND g.tgfoid=to_regprocedure('public.protect_job_retention_event()'))
            OR (t.relname='job_retention_events' AND g.tgname='job_retention_events_no_truncate'
                AND g.tgtype=34 AND g.tgattr=''::int2vector
                AND g.tgfoid=to_regprocedure('public.protect_job_retention_event()'))
        ),false)))
    OR ((SELECT present FROM retention_present) AND (
        (SELECT count(*) FROM allowed_tables WHERE relname IN
            ('job_retention_holds','job_retention_events'))<>2
        OR to_regprocedure('public.change_job_retention_hold(uuid,uuid,bigint,uuid,text,text)') IS NULL
        OR to_regprocedure('public.guard_job_retention_delete()') IS NULL
        OR to_regprocedure('public.guard_held_job_source()') IS NULL
        OR to_regprocedure('public.protect_job_retention_event()') IS NULL
        OR EXISTS(SELECT 1 FROM (VALUES
            ('jobs','jobs_retention_delete_guard'),('jobs','jobs_retention_truncate_guard'),
            ('jobs','jobs_held_source_guard'),('job_retention_events','job_retention_events_append_only'),
            ('job_retention_events','job_retention_events_no_truncate')
        ) required(parent,name) WHERE NOT EXISTS(
            SELECT 1 FROM pg_trigger g JOIN allowed_tables t ON t.oid=g.tgrelid
            WHERE t.relname=required.parent AND g.tgname=required.name AND NOT g.tgisinternal))
    ))
    OR EXISTS(SELECT 1 FROM pg_policy p JOIN allowed_tables t ON t.oid=p.polrelid)
    OR EXISTS(SELECT 1 FROM pg_event_trigger)
    OR EXISTS(SELECT 1 FROM pg_foreign_server)
    OR EXISTS(SELECT 1 FROM pg_foreign_data_wrapper)
    OR EXISTS(SELECT 1 FROM pg_operator x JOIN pg_namespace n ON n.oid=x.oprnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_opclass x JOIN pg_namespace n ON n.oid=x.opcnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_opfamily x JOIN pg_namespace n ON n.oid=x.opfnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_collation x JOIN pg_namespace n ON n.oid=x.collnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_conversion x JOIN pg_namespace n ON n.oid=x.connamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_statistic_ext x JOIN pg_namespace n ON n.oid=x.stxnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_ts_config x JOIN pg_namespace n ON n.oid=x.cfgnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_ts_dict x JOIN pg_namespace n ON n.oid=x.dictnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_ts_parser x JOIN pg_namespace n ON n.oid=x.prsnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_ts_template x JOIN pg_namespace n ON n.oid=x.tmplnamespace WHERE n.nspname='public')
    OR EXISTS(SELECT 1 FROM pg_extension WHERE extname<>'plpgsql');
