-- Cluster roles are created by Rust after collision/attribute checks. This SQL
-- is deliberately outside immutable application migrations 0001--0005.
-- PostgreSQL >=16 required for explicit SET/INHERIT membership options.
GRANT ocr_owner TO ocr_migrator WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
REVOKE ALL ON SCHEMA public FROM PUBLIC,ocr_owner,ocr_migrator,ocr_api,ocr_worker,ocr_maintenance,ocr_backup;
GRANT USAGE, CREATE ON SCHEMA public TO ocr_owner;
GRANT USAGE ON SCHEMA public TO ocr_api, ocr_worker, ocr_maintenance, ocr_backup;
-- Must be global: a per-schema revoke cannot undo the global PUBLIC default.
ALTER DEFAULT PRIVILEGES FOR ROLE ocr_owner REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER ROLE ocr_migrator SET search_path = public, pg_catalog;
ALTER ROLE ocr_api SET search_path = pg_catalog, public;
ALTER ROLE ocr_worker SET search_path = pg_catalog, public;
ALTER ROLE ocr_maintenance SET search_path = pg_catalog, public;
ALTER ROLE ocr_backup SET search_path = pg_catalog, public;
