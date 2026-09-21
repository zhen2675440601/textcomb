ALTER TABLE analysis_jobs
    ADD COLUMN analysis_profile VARCHAR(24) NOT NULL DEFAULT 'general'
    CHECK (analysis_profile IN ('general', 'academic', 'financial'));
