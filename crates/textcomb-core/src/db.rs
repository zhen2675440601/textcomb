use crate::{
    error::{CoreError, CoreResult, ErrorCode},
    prompts,
    provider::TokenUsage,
};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction, postgres::PgPoolOptions};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use textcomb_domain::{EvidenceRef, Issue, ReportV1};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub user_id: Uuid,
    pub document_id: Uuid,
    pub model_profile_id: Uuid,
    pub prompt_version_id: Option<Uuid>,
    pub attempt_count: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DocumentRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub original_name: String,
    pub media_type: String,
    pub document_format: String,
    pub size_bytes: i64,
    pub storage_path: Option<String>,
    pub extracted_text: Option<String>,
    pub char_count: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModelProfileRecord {
    pub id: Uuid,
    pub owner_id: Option<Uuid>,
    pub name: String,
    pub provider_kind: String,
    pub base_url: String,
    pub api_key_ciphertext: Vec<u8>,
    pub candidate_model: String,
    pub verifier_model: String,
    pub enabled: bool,
    pub is_reference: bool,
    pub max_concurrency: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PromptRecord {
    pub id: Uuid,
    pub version: String,
    pub candidate_template: String,
    pub verifier_template: String,
    pub confirmed_threshold: i16,
    pub suspected_threshold: i16,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReportRecord {
    pub id: Uuid,
    pub job_id: Uuid,
    pub user_id: Uuid,
    pub schema_version: String,
    pub document_name: String,
    pub content_json: Value,
    pub markdown: String,
    pub pdf_path: Option<String>,
    pub complete: bool,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub enum CleanupTarget {
    Document(PathBuf),
    Report { id: Uuid, path: Option<PathBuf> },
}

#[derive(Debug, Clone)]
pub enum CancellationOutcome {
    Requested,
    Finalized(Option<PathBuf>),
}

pub async fn connect(database_url: &str) -> CoreResult<PgPool> {
    PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await
        .map_err(CoreError::Database)
}

pub async fn migrate(pool: &PgPool) -> CoreResult<()> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?;
    ensure_default_prompt(pool).await
}

pub async fn ensure_default_prompt(pool: &PgPool) -> CoreResult<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query("UPDATE prompt_versions SET active = FALSE WHERE active AND version <> $1")
        .bind(prompts::DEFAULT_PROMPT_VERSION)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        r#"
        INSERT INTO prompt_versions(
            id, version, candidate_template, verifier_template,
            confirmed_threshold, suspected_threshold, active
        )
        VALUES($1, $2, $3, $4, $5, $6, TRUE)
        ON CONFLICT(version) DO UPDATE SET
            candidate_template = EXCLUDED.candidate_template,
            verifier_template = EXCLUDED.verifier_template,
            confirmed_threshold = EXCLUDED.confirmed_threshold,
            suspected_threshold = EXCLUDED.suspected_threshold,
            active = TRUE
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(prompts::DEFAULT_PROMPT_VERSION)
    .bind(prompts::CANDIDATE_SYSTEM_PROMPT)
    .bind(prompts::VERIFIER_SYSTEM_PROMPT)
    .bind(i16::from(prompts::CONFIRMED_THRESHOLD))
    .bind(i16::from(prompts::SUSPECTED_THRESHOLD))
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn claim_job(pool: &PgPool, worker_id: &str) -> CoreResult<Option<ClaimedJob>> {
    let job = sqlx::query_as::<_, ClaimedJob>(
        r#"
        WITH candidate AS (
            SELECT id
            FROM analysis_jobs
            WHERE status = 'queued'
            ORDER BY created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE analysis_jobs AS job
        SET status = 'extracting',
            stage = 'extracting',
            progress = 2,
            worker_id = $1,
            lease_until = now() + interval '2 minutes',
            heartbeat_at = now(),
            started_at = COALESCE(started_at, now()),
            attempt_count = attempt_count + 1
        FROM candidate
        WHERE job.id = candidate.id
        RETURNING job.id, job.user_id, job.document_id, job.model_profile_id,
                  job.prompt_version_id, job.attempt_count
        "#,
    )
    .bind(worker_id)
    .fetch_optional(pool)
    .await?;
    Ok(job)
}

pub async fn recover_expired_leases(pool: &PgPool) -> CoreResult<u64> {
    let result = sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET status = 'queued',
            stage = 'queued',
            worker_id = NULL,
            lease_until = NULL,
            heartbeat_at = NULL,
            error_code = 'WORKER_LEASE_EXPIRED',
            error_message = 'Worker 租约过期，任务已重新排队'
        WHERE status IN ('extracting', 'analyzing', 'verifying', 'merging', 'rendering')
          AND lease_until < now()
          AND timeout_at > now()
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn heartbeat(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    status: &str,
    stage: &str,
    progress: i16,
    completed_chunks: i32,
) -> CoreResult<()> {
    let result = sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET status = $3,
            stage = $4,
            progress = $5,
            completed_chunks = $6,
            lease_until = now() + interval '2 minutes',
            heartbeat_at = now()
        WHERE id = $1 AND worker_id = $2
          AND status IN ('extracting', 'analyzing', 'verifying', 'merging', 'rendering')
        "#,
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(status)
    .bind(stage)
    .bind(progress.clamp(0, 100))
    .bind(completed_chunks)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        if is_cancel_requested(pool, job_id).await? {
            return Err(CoreError::public(
                ErrorCode::JobCancelled,
                "任务已由用户取消",
            ));
        }
        return Err(CoreError::public(ErrorCode::Conflict, "任务租约已失效"));
    }
    Ok(())
}

pub async fn refresh_lease(pool: &PgPool, job_id: Uuid, worker_id: &str) -> CoreResult<()> {
    let result = sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET lease_until = now() + interval '2 minutes', heartbeat_at = now()
        WHERE id = $1 AND worker_id = $2
          AND status IN ('extracting', 'analyzing', 'verifying', 'merging', 'rendering')
        "#,
    )
    .bind(job_id)
    .bind(worker_id)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(CoreError::public(ErrorCode::Conflict, "任务租约已失效"));
    }
    Ok(())
}

pub async fn is_cancel_requested(pool: &PgPool, job_id: Uuid) -> CoreResult<bool> {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM analysis_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_optional(pool)
            .await?;
    Ok(matches!(
        status.as_deref(),
        Some("cancel_requested" | "cancelled")
    ))
}

pub async fn request_cancellation(
    pool: &PgPool,
    job_id: Uuid,
    user_id: Uuid,
) -> CoreResult<CancellationOutcome> {
    let mut transaction = pool.begin().await?;
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT job.status, document.storage_path
        FROM analysis_jobs AS job
        JOIN documents AS document ON document.id = job.document_id
        WHERE job.id = $1 AND job.user_id = $2
        FOR UPDATE OF job, document
        "#,
    )
    .bind(job_id)
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((status, storage_path)) = row else {
        return Err(CoreError::public(ErrorCode::NotFound, "分析任务不存在"));
    };

    let outcome = match status.as_str() {
        "queued" => {
            sqlx::query(
                r#"
                UPDATE analysis_jobs
                SET status = 'cancelled', stage = 'cancelled', lease_until = NULL,
                    completed_at = now(), error_code = 'JOB_CANCELLED',
                    error_message = '任务已由用户取消'
                WHERE id = $1
                "#,
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                UPDATE analysis_chunks
                SET content = NULL, candidate_output = NULL, verifier_output = NULL,
                    updated_at = now()
                WHERE job_id = $1
                "#,
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                UPDATE documents AS document
                SET extracted_text = NULL,
                    input_expires_at = CASE
                        WHEN document.storage_path IS NULL THEN NULL
                        ELSE now()
                    END
                FROM analysis_jobs AS job
                WHERE job.id = $1 AND document.id = job.document_id
                "#,
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await?;
            CancellationOutcome::Finalized(storage_path.map(PathBuf::from))
        }
        "extracting" | "analyzing" | "verifying" | "merging" | "rendering" => {
            sqlx::query(
                "UPDATE analysis_jobs SET status = 'cancel_requested', stage = 'cancel_requested' WHERE id = $1",
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await?;
            CancellationOutcome::Requested
        }
        "cancel_requested" => CancellationOutcome::Requested,
        "cancelled" => CancellationOutcome::Finalized(storage_path.map(PathBuf::from)),
        _ => {
            return Err(CoreError::public(
                ErrorCode::Conflict,
                "任务当前状态不能取消",
            ));
        }
    };
    transaction.commit().await?;
    Ok(outcome)
}

pub async fn load_document(pool: &PgPool, document_id: Uuid) -> CoreResult<DocumentRecord> {
    sqlx::query_as::<_, DocumentRecord>(
        r#"
        SELECT id, user_id, original_name, media_type, document_format,
               size_bytes, storage_path, extracted_text, char_count
        FROM documents WHERE id = $1
        "#,
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::NotFound, "任务对应的文档不存在"))
}

pub async fn load_model_profile(
    pool: &PgPool,
    profile_id: Uuid,
    user_id: Uuid,
) -> CoreResult<ModelProfileRecord> {
    sqlx::query_as::<_, ModelProfileRecord>(
        r#"
        SELECT id, owner_id, name, provider_kind, base_url, api_key_ciphertext,
               candidate_model, verifier_model, enabled, is_reference, max_concurrency
        FROM model_profiles
        WHERE id = $1 AND enabled AND (owner_id IS NULL OR owner_id = $2)
        "#,
    )
    .bind(profile_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::NotFound, "模型配置不存在或已停用"))
}

pub async fn load_active_prompt(pool: &PgPool) -> CoreResult<PromptRecord> {
    sqlx::query_as::<_, PromptRecord>(
        r#"
        SELECT id, version, candidate_template, verifier_template,
               confirmed_threshold, suspected_threshold
        FROM prompt_versions WHERE active LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::ConfigurationInvalid, "没有启用的提示词版本"))
}

pub async fn load_prompt(pool: &PgPool, prompt_id: Uuid) -> CoreResult<PromptRecord> {
    sqlx::query_as::<_, PromptRecord>(
        r#"
        SELECT id, version, candidate_template, verifier_template,
               confirmed_threshold, suspected_threshold
        FROM prompt_versions WHERE id = $1
        "#,
    )
    .bind(prompt_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::ConfigurationInvalid, "任务提示词版本不存在"))
}

pub async fn load_evidence(pool: &PgPool) -> CoreResult<HashMap<String, EvidenceRef>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        title: String,
        revision: String,
        source_url: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, title, revision, source_url FROM evidence_entries WHERE active",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let id = row.id.clone();
            (
                id,
                EvidenceRef {
                    source_id: row.id,
                    title: row.title,
                    revision: row.revision,
                    source_url: row.source_url,
                },
            )
        })
        .collect())
}

pub async fn save_extracted_document(
    pool: &PgPool,
    document_id: Uuid,
    text: &str,
    char_count: usize,
    expires_at: OffsetDateTime,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE documents SET extracted_text = $2, char_count = $3, input_expires_at = $4 WHERE id = $1",
    )
    .bind(document_id)
    .bind(text)
    .bind(char_count as i32)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_configuration(
    pool: &PgPool,
    job_id: Uuid,
    prompt_version_id: Uuid,
    model_snapshot: &Value,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE analysis_jobs SET prompt_version_id = $2, model_snapshot = $3 WHERE id = $1",
    )
    .bind(job_id)
    .bind(prompt_version_id)
    .bind(model_snapshot)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn replace_chunks(
    pool: &PgPool,
    job_id: Uuid,
    chunks: &[crate::analysis::AnalysisChunk],
) -> CoreResult<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM analysis_chunks WHERE job_id = $1 AND chunk_index >= $2")
        .bind(job_id)
        .bind(chunks.len() as i32)
        .execute(&mut *transaction)
        .await?;
    for chunk in chunks {
        let existing: Option<(i32, i32, Option<String>, String, bool)> = sqlx::query_as(
            r#"
            SELECT source_start, source_end, content, status,
                   candidate_output IS NOT NULL AND verifier_output IS NOT NULL AS has_outputs
            FROM analysis_chunks WHERE job_id = $1 AND chunk_index = $2
            "#,
        )
        .bind(job_id)
        .bind(chunk.index as i32)
        .fetch_optional(&mut *transaction)
        .await?;
        let reusable = existing.as_ref().is_some_and(
            |(source_start, source_end, content, status, has_outputs)| {
                *source_start == chunk.source_start as i32
                    && *source_end == chunk.source_end as i32
                    && content.as_deref() == Some(chunk.text.as_str())
                    && status == "completed"
                    && *has_outputs
            },
        );
        if existing.is_none() {
            sqlx::query(
                r#"
                INSERT INTO analysis_chunks(
                    id, job_id, chunk_index, source_start, source_end, content
                ) VALUES($1, $2, $3, $4, $5, $6)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(job_id)
            .bind(chunk.index as i32)
            .bind(chunk.source_start as i32)
            .bind(chunk.source_end as i32)
            .bind(&chunk.text)
            .execute(&mut *transaction)
            .await?;
        } else if !reusable {
            sqlx::query(
                r#"
                UPDATE analysis_chunks
                SET source_start = $3, source_end = $4, content = $5,
                    status = 'pending', candidate_output = NULL, verifier_output = NULL,
                    error_code = NULL, updated_at = now()
                WHERE job_id = $1 AND chunk_index = $2
                "#,
            )
            .bind(job_id)
            .bind(chunk.index as i32)
            .bind(chunk.source_start as i32)
            .bind(chunk.source_end as i32)
            .bind(&chunk.text)
            .execute(&mut *transaction)
            .await?;
        }
    }
    let completed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM analysis_chunks WHERE job_id = $1 AND status = 'completed'",
    )
    .bind(job_id)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query("UPDATE analysis_jobs SET total_chunks = $2, completed_chunks = $3 WHERE id = $1")
        .bind(job_id)
        .bind(chunks.len() as i32)
        .bind(completed as i32)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
pub struct ReusableChunkOutput {
    pub chunk_index: i32,
    pub candidate_output: Value,
    pub verifier_output: Value,
}

pub async fn load_reusable_chunk_outputs(
    pool: &PgPool,
    job_id: Uuid,
) -> CoreResult<Vec<ReusableChunkOutput>> {
    Ok(sqlx::query_as::<_, ReusableChunkOutput>(
        r#"
        SELECT chunk_index, candidate_output, verifier_output
        FROM analysis_chunks
        WHERE job_id = $1 AND status = 'completed'
          AND candidate_output IS NOT NULL AND verifier_output IS NOT NULL
        ORDER BY chunk_index
        "#,
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?)
}

pub async fn mark_chunk_analyzing(
    pool: &PgPool,
    job_id: Uuid,
    chunk_index: usize,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE analysis_chunks SET status = 'analyzing', error_code = NULL, updated_at = now() WHERE job_id = $1 AND chunk_index = $2",
    )
    .bind(job_id)
    .bind(chunk_index as i32)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_chunk_verifying(
    pool: &PgPool,
    job_id: Uuid,
    chunk_index: usize,
) -> CoreResult<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "UPDATE analysis_chunks SET status = 'verifying', updated_at = now() WHERE job_id = $1 AND chunk_index = $2",
    )
    .bind(job_id)
    .bind(chunk_index as i32)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "UPDATE analysis_jobs SET status = 'verifying', stage = 'verifying' WHERE id = $1 AND status IN ('analyzing','verifying')",
    )
    .bind(job_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn mark_chunk_failed(
    pool: &PgPool,
    job_id: Uuid,
    chunk_index: usize,
    error: &CoreError,
) -> CoreResult<()> {
    sqlx::query(
        r#"
        UPDATE analysis_chunks
        SET status = 'failed', retry_count = retry_count + 1,
            error_code = $3, updated_at = now()
        WHERE job_id = $1 AND chunk_index = $2
        "#,
    )
    .bind(job_id)
    .bind(chunk_index as i32)
    .bind(error.code().as_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn save_chunk_outputs(
    pool: &PgPool,
    job_id: Uuid,
    chunk_index: usize,
    candidate_output: &Value,
    verifier_output: &Value,
) -> CoreResult<()> {
    sqlx::query(
        r#"
        UPDATE analysis_chunks
        SET status = 'completed',
            candidate_output = $3,
            verifier_output = $4,
            updated_at = now()
        WHERE job_id = $1 AND chunk_index = $2
        "#,
    )
    .bind(job_id)
    .bind(chunk_index as i32)
    .bind(candidate_output)
    .bind(verifier_output)
    .execute(pool)
    .await?;
    Ok(())
}

pub struct UsageRecord<'a> {
    pub job_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub pass: &'a str,
    pub provider_kind: &'a str,
    pub model: &'a str,
    pub usage: &'a TokenUsage,
    pub duration_ms: i64,
    pub attempt: i32,
    pub success: bool,
    pub error_code: Option<&'a str>,
}

pub async fn record_usage(pool: &PgPool, record: UsageRecord<'_>) -> CoreResult<()> {
    sqlx::query(
        r#"
        INSERT INTO model_usage(
            job_id, chunk_id, pass, provider_kind, model,
            prompt_tokens, completion_tokens, duration_ms, attempt, success, error_code
        ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        "#,
    )
    .bind(record.job_id)
    .bind(record.chunk_id)
    .bind(record.pass)
    .bind(record.provider_kind)
    .bind(record.model)
    .bind(record.usage.prompt_tokens)
    .bind(record.usage.completion_tokens)
    .bind(record.duration_ms)
    .bind(record.attempt)
    .bind(record.success)
    .bind(record.error_code)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn save_report(
    pool: &PgPool,
    report: &ReportV1,
    user_id: Uuid,
    markdown: &str,
    pdf_path: Option<&str>,
    expires_at: OffsetDateTime,
) -> CoreResult<()> {
    let mut transaction = pool.begin().await?;
    save_report_in_transaction(
        &mut transaction,
        report,
        user_id,
        markdown,
        pdf_path,
        expires_at,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn save_report_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    report: &ReportV1,
    user_id: Uuid,
    markdown: &str,
    pdf_path: Option<&str>,
    expires_at: OffsetDateTime,
) -> CoreResult<()> {
    let content_json = serde_json::to_value(report)?;
    sqlx::query(
        r#"
        INSERT INTO reports(
            id, job_id, user_id, schema_version, document_name,
            content_json, markdown, pdf_path, complete, created_at, expires_at
        ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,TRUE,$9,$10)
        "#,
    )
    .bind(report.report_id)
    .bind(report.job_id)
    .bind(user_id)
    .bind(&report.schema)
    .bind(&report.document.original_name)
    .bind(&content_json)
    .bind(markdown)
    .bind(pdf_path)
    .bind(report.generated_at)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await?;
    for issue in &report.issues {
        insert_issue(transaction, report.report_id, issue).await?;
    }
    sqlx::query(
        r#"
        UPDATE analysis_chunks
        SET content = NULL, candidate_output = NULL, verifier_output = NULL,
            updated_at = now()
        WHERE job_id = $1
        "#,
    )
    .bind(report.job_id)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        r#"
        UPDATE documents AS document
        SET extracted_text = NULL,
            input_expires_at = CASE
                WHEN document.storage_path IS NULL THEN NULL
                ELSE now()
            END
        FROM analysis_jobs AS job
        WHERE job.id = $1 AND document.id = job.document_id
        "#,
    )
    .bind(report.job_id)
    .execute(&mut **transaction)
    .await?;
    let completed = sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET status = 'completed', stage = 'completed', progress = 100,
            report_id = $2, completed_at = now(), lease_until = NULL,
            heartbeat_at = now(), error_code = NULL, error_message = NULL
        WHERE id = $1
          AND status IN ('analyzing', 'verifying', 'merging', 'rendering')
        "#,
    )
    .bind(report.job_id)
    .bind(report.report_id)
    .execute(&mut **transaction)
    .await?;
    if completed.rows_affected() != 1 {
        return Err(CoreError::public(
            ErrorCode::JobCancelled,
            "任务已由用户取消",
        ));
    }
    Ok(())
}

async fn insert_issue(
    transaction: &mut Transaction<'_, Postgres>,
    report_id: Uuid,
    issue: &Issue,
) -> CoreResult<()> {
    sqlx::query(
        r#"
        INSERT INTO issues(
            id, report_id, category, subtype, issue_level, source_location,
            original_text, reason, suggestion, confidence, evidence_refs
        ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        "#,
    )
    .bind(issue.id)
    .bind(report_id)
    .bind(issue.category.as_str())
    .bind(issue.grammar_subtype.map(|subtype| subtype.as_str()))
    .bind(issue.level.as_str())
    .bind(serde_json::to_value(&issue.location)?)
    .bind(&issue.original_text)
    .bind(&issue.reason)
    .bind(&issue.suggestion)
    .bind(i16::from(issue.confidence))
    .bind(serde_json::to_value(&issue.evidence_refs)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub async fn confirm_document_file_removed(pool: &PgPool, path: &Path) -> CoreResult<()> {
    sqlx::query(
        "UPDATE documents SET storage_path = NULL, input_expires_at = NULL WHERE storage_path = $1",
    )
    .bind(path.to_string_lossy().as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_failed(pool: &PgPool, job_id: Uuid, error: &CoreError) -> CoreResult<()> {
    sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET status = 'failed', stage = 'failed', lease_until = NULL,
            completed_at = now(), error_code = $2, error_message = $3
        WHERE id = $1
          AND status NOT IN ('cancel_requested', 'cancelled', 'completed', 'expired')
        "#,
    )
    .bind(job_id)
    .bind(error.code().as_str())
    .bind(error.safe_message().as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn finalize_cancelled(pool: &PgPool, job_id: Uuid) -> CoreResult<Option<PathBuf>> {
    let mut transaction = pool.begin().await?;
    let storage_path: Option<String> = sqlx::query_scalar(
        r#"
        SELECT document.storage_path
        FROM analysis_jobs AS job
        JOIN documents AS document ON document.id = job.document_id
        WHERE job.id = $1
        FOR UPDATE OF job, document
        "#,
    )
    .bind(job_id)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        UPDATE analysis_jobs
        SET status = 'cancelled', stage = 'cancelled', lease_until = NULL,
            completed_at = now(), error_code = 'JOB_CANCELLED',
            error_message = '任务已由用户取消'
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        UPDATE analysis_chunks
        SET content = NULL, candidate_output = NULL, verifier_output = NULL,
            updated_at = now()
        WHERE job_id = $1
        "#,
    )
    .bind(job_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        UPDATE documents AS document
        SET extracted_text = NULL,
            input_expires_at = CASE
                WHEN document.storage_path IS NULL THEN NULL
                ELSE now()
            END
        FROM analysis_jobs AS job
        WHERE job.id = $1 AND document.id = job.document_id
        "#,
    )
    .bind(job_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(storage_path.map(PathBuf::from))
}

pub async fn load_report(
    pool: &PgPool,
    report_id: Uuid,
    user_id: Uuid,
) -> CoreResult<ReportRecord> {
    sqlx::query_as::<_, ReportRecord>(
        r#"
        SELECT id, job_id, user_id, schema_version, document_name,
               content_json, markdown, pdf_path, complete, created_at, expires_at
        FROM reports WHERE id = $1 AND user_id = $2 AND expires_at > now()
        "#,
    )
    .bind(report_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::NotFound, "报告不存在或已过期"))
}

pub async fn cleanup_expired(pool: &PgPool) -> CoreResult<Vec<CleanupTarget>> {
    let paths: Vec<Option<String>> = sqlx::query_scalar(
        r#"
        SELECT storage_path FROM documents
        WHERE input_expires_at IS NOT NULL AND input_expires_at < now()
        "#,
    )
    .fetch_all(pool)
    .await?;
    sqlx::query(
        r#"
        UPDATE analysis_chunks AS chunk
        SET content = NULL, candidate_output = NULL, verifier_output = NULL,
            updated_at = now()
        FROM analysis_jobs AS job
        JOIN documents AS document ON document.id = job.document_id
        WHERE chunk.job_id = job.id
          AND document.input_expires_at IS NOT NULL
          AND document.input_expires_at < now()
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        UPDATE documents SET extracted_text = NULL
        WHERE input_expires_at IS NOT NULL AND input_expires_at < now()
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        UPDATE documents SET input_expires_at = NULL
        WHERE storage_path IS NULL
          AND input_expires_at IS NOT NULL
          AND input_expires_at < now()
        "#,
    )
    .execute(pool)
    .await?;
    let reports: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, pdf_path FROM reports WHERE expires_at < now()")
            .fetch_all(pool)
            .await?;
    let mut targets: Vec<CleanupTarget> = paths
        .into_iter()
        .flatten()
        .map(|path| CleanupTarget::Document(PathBuf::from(path)))
        .collect();
    targets.extend(reports.into_iter().map(|(id, path)| CleanupTarget::Report {
        id,
        path: path.map(PathBuf::from),
    }));
    Ok(targets)
}

pub async fn confirm_cleanup_target(pool: &PgPool, target: &CleanupTarget) -> CoreResult<()> {
    match target {
        CleanupTarget::Document(path) => confirm_document_file_removed(pool, path).await,
        CleanupTarget::Report { id, .. } => {
            sqlx::query("DELETE FROM reports WHERE id = $1 AND expires_at < now()")
                .bind(id)
                .execute(pool)
                .await?;
            Ok(())
        }
    }
}
