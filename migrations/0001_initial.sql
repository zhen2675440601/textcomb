CREATE TABLE users (
    id UUID PRIMARY KEY,
    username VARCHAR(64) NOT NULL,
    password_hash TEXT NOT NULL,
    role VARCHAR(24) NOT NULL CHECK (role IN ('super_admin', 'user')),
    status VARCHAR(24) NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_username_lower_uq ON users (lower(username));

CREATE TABLE sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_agent_hash BYTEA,
    ip_address INET
);
CREATE INDEX sessions_user_id_idx ON sessions(user_id);
CREATE INDEX sessions_expires_at_idx ON sessions(expires_at);

CREATE TABLE login_attempts (
    id BIGSERIAL PRIMARY KEY,
    username VARCHAR(64) NOT NULL,
    ip_address INET,
    success BOOLEAN NOT NULL,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX login_attempts_lookup_idx
    ON login_attempts(lower(username), ip_address, attempted_at DESC);

CREATE TABLE model_profiles (
    id UUID PRIMARY KEY,
    owner_id UUID REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(100) NOT NULL,
    provider_kind VARCHAR(32) NOT NULL DEFAULT 'openai_compatible',
    base_url TEXT NOT NULL,
    api_key_ciphertext BYTEA NOT NULL,
    candidate_model VARCHAR(160) NOT NULL,
    verifier_model VARCHAR(160) NOT NULL,
    max_concurrency INTEGER NOT NULL DEFAULT 10 CHECK (max_concurrency BETWEEN 1 AND 100),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    is_reference BOOLEAN NOT NULL DEFAULT FALSE,
    disclosure_accepted_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX model_profiles_owner_name_uq
    ON model_profiles(COALESCE(owner_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(name));

CREATE TABLE documents (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    original_name TEXT NOT NULL,
    media_type VARCHAR(160) NOT NULL,
    document_format VARCHAR(16) NOT NULL CHECK (document_format IN ('txt', 'docx', 'pdf')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0 AND size_bytes <= 20971520),
    storage_path TEXT,
    extracted_text TEXT,
    char_count INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    input_expires_at TIMESTAMPTZ
);
CREATE INDEX documents_user_created_idx ON documents(user_id, created_at DESC);

CREATE TABLE prompt_versions (
    id UUID PRIMARY KEY,
    version VARCHAR(80) NOT NULL UNIQUE,
    candidate_template TEXT NOT NULL,
    verifier_template TEXT NOT NULL,
    confirmed_threshold SMALLINT NOT NULL DEFAULT 80,
    suspected_threshold SMALLINT NOT NULL DEFAULT 50,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    active BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE UNIQUE INDEX prompt_versions_one_active_idx ON prompt_versions(active) WHERE active;

CREATE TABLE evidence_entries (
    id VARCHAR(80) PRIMARY KEY,
    title TEXT NOT NULL,
    revision VARCHAR(80) NOT NULL,
    source_url TEXT NOT NULL,
    redistributable BOOLEAN NOT NULL DEFAULT FALSE,
    prompt_excerpt TEXT,
    content_sha256 VARCHAR(64),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE analysis_jobs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    model_profile_id UUID NOT NULL REFERENCES model_profiles(id),
    prompt_version_id UUID REFERENCES prompt_versions(id),
    status VARCHAR(32) NOT NULL CHECK (status IN (
        'queued', 'extracting', 'analyzing', 'verifying', 'merging',
        'rendering', 'completed', 'failed', 'cancel_requested', 'cancelled', 'expired'
    )),
    stage VARCHAR(64) NOT NULL DEFAULT 'queued',
    progress SMALLINT NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 100),
    idempotency_key VARCHAR(160),
    worker_id VARCHAR(160),
    lease_until TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    total_chunks INTEGER NOT NULL DEFAULT 0,
    completed_chunks INTEGER NOT NULL DEFAULT 0,
    error_code VARCHAR(80),
    error_message TEXT,
    report_id UUID,
    model_snapshot JSONB,
    analyzer_version VARCHAR(80) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    timeout_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX analysis_jobs_idempotency_uq
    ON analysis_jobs(user_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX analysis_jobs_queue_idx ON analysis_jobs(status, created_at);
CREATE INDEX analysis_jobs_user_created_idx ON analysis_jobs(user_id, created_at DESC);

CREATE TABLE analysis_chunks (
    id UUID PRIMARY KEY,
    job_id UUID NOT NULL REFERENCES analysis_jobs(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    source_start INTEGER NOT NULL,
    source_end INTEGER NOT NULL,
    overlap_start INTEGER,
    overlap_end INTEGER,
    content TEXT,
    status VARCHAR(24) NOT NULL DEFAULT 'pending' CHECK (status IN (
        'pending', 'analyzing', 'verifying', 'completed', 'failed'
    )),
    candidate_output JSONB,
    verifier_output JSONB,
    retry_count INTEGER NOT NULL DEFAULT 0,
    error_code VARCHAR(80),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(job_id, chunk_index)
);
CREATE INDEX analysis_chunks_job_idx ON analysis_chunks(job_id, chunk_index);

CREATE TABLE reports (
    id UUID PRIMARY KEY,
    job_id UUID NOT NULL UNIQUE REFERENCES analysis_jobs(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    schema_version VARCHAR(64) NOT NULL DEFAULT 'textcomb.report.v1',
    document_name TEXT NOT NULL,
    content_json JSONB NOT NULL,
    markdown TEXT NOT NULL,
    pdf_path TEXT,
    complete BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX reports_user_created_idx ON reports(user_id, created_at DESC);
CREATE INDEX reports_expires_at_idx ON reports(expires_at);

ALTER TABLE analysis_jobs
    ADD CONSTRAINT analysis_jobs_report_fk
    FOREIGN KEY (report_id) REFERENCES reports(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE issues (
    id UUID PRIMARY KEY,
    report_id UUID NOT NULL REFERENCES reports(id) ON DELETE CASCADE,
    category VARCHAR(24) NOT NULL CHECK (category IN ('typo', 'punctuation', 'grammar', 'paragraph')),
    subtype VARCHAR(64),
    issue_level VARCHAR(24) NOT NULL CHECK (issue_level IN ('confirmed', 'suspected')),
    source_location JSONB NOT NULL,
    original_text TEXT NOT NULL,
    reason TEXT NOT NULL,
    suggestion TEXT NOT NULL,
    confidence SMALLINT NOT NULL CHECK (confidence BETWEEN 0 AND 100),
    evidence_refs JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX issues_report_idx ON issues(report_id, category, issue_level);

CREATE TABLE issue_feedback (
    id UUID PRIMARY KEY,
    issue_id UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    verdict VARCHAR(24) NOT NULL CHECK (verdict IN ('correct', 'incorrect', 'disputed')),
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(issue_id, user_id)
);

CREATE TABLE model_usage (
    id BIGSERIAL PRIMARY KEY,
    job_id UUID NOT NULL REFERENCES analysis_jobs(id) ON DELETE CASCADE,
    chunk_id UUID REFERENCES analysis_chunks(id) ON DELETE SET NULL,
    pass VARCHAR(24) NOT NULL CHECK (pass IN ('candidate', 'verification', 'repair')),
    provider_kind VARCHAR(32) NOT NULL,
    model VARCHAR(160) NOT NULL,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    duration_ms BIGINT NOT NULL,
    attempt INTEGER NOT NULL,
    success BOOLEAN NOT NULL,
    error_code VARCHAR(80),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX model_usage_job_idx ON model_usage(job_id);

CREATE TABLE audit_events (
    id BIGSERIAL PRIMARY KEY,
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    action VARCHAR(100) NOT NULL,
    target_type VARCHAR(80),
    target_id UUID,
    request_id VARCHAR(100),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_events_created_idx ON audit_events(created_at DESC);
