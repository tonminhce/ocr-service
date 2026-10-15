-- Preserve existing OCR-only jobs and freeze the requested extraction behavior
-- on each submission, rather than consulting a worker environment setting.
ALTER TABLE jobs
    ADD COLUMN extraction_profile TEXT NOT NULL DEFAULT 'none'
    CHECK (extraction_profile IN ('none', 'vi_rental_contract_v1'));
