use crate::{
    problem::{ApiError, ApiResult},
    session::AuthUser,
    state::AppState,
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, time::Duration};
use textcomb_core::{CoreError, ErrorCode, db};
use textcomb_domain::{ANALYZER_VERSION, AnalysisProfile};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/analyses", get(list).post(create))
        .route("/analyses/{id}", get(get_one).delete(remove))
        .route("/analyses/{id}/events", get(events))
        .route("/analyses/{id}/cancel", post(cancel))
        .route("/analyses/{id}/retry", post(retry))
}

#[derive(Debug, Serialize, sqlx::FromRow, Clone)]
pub struct AnalysisResponse {
    pub id: Uuid,
    pub document_id: Uuid,
    pub model_profile_id: Uuid,
    pub analysis_profile: String,
    pub status: String,
    pub stage: String,
    pub progress: i16,
    pub total_chunks: i32,
    pub completed_chunks: i32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub report_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<AnalysisResponse>>> {
    let jobs = sqlx::query_as::<_, AnalysisResponse>(
        r#"
        SELECT id, document_id, model_profile_id, analysis_profile, status, stage, progress,
               total_chunks, completed_chunks, error_code, error_message,
               report_id, created_at, started_at, completed_at
        FROM analysis_jobs WHERE user_id = $1
        ORDER BY created_at DESC LIMIT 200
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(jobs))
}

#[derive(Debug, Deserialize)]
struct CreateAnalysisRequest {
    document_id: Uuid,
    model_profile_id: Uuid,
    #[serde(default)]
    analysis_profile: Option<AnalysisProfile>,
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    headers: HeaderMap,
    Json(input): Json<CreateAnalysisRequest>,
) -> ApiResult<(StatusCode, Json<AnalysisResponse>)> {
    let idempotency_key = parse_idempotency_key(&headers)?;
    if let Some(key) = idempotency_key.as_ref()
        && let Some(existing) = fetch_by_idempotency(&state, user.id, key).await?
    {
        ensure_same_idempotent_request(&existing, &input)?;
        return Ok((StatusCode::OK, Json(existing)));
    }
    let document_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM documents
            WHERE id = $1 AND user_id = $2 AND storage_path IS NOT NULL
              AND input_expires_at > now()
        )
        "#,
    )
    .bind(input.document_id)
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;
    if !document_exists {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "文档不存在、已删除或已完成过分析",
        )));
    }
    let profile_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM model_profiles
            WHERE id = $1 AND enabled AND (owner_id IS NULL OR owner_id = $2)
        )
        "#,
    )
    .bind(input.model_profile_id)
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;
    if !profile_exists {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "模型配置不存在或已停用",
        )));
    }
    let analysis_profile = input.analysis_profile.unwrap_or_default();
    let model_record = db::load_model_profile(&state.pool, input.model_profile_id, user.id).await?;
    let prompt_record = db::load_active_prompt(&state.pool).await?;
    let model_snapshot = serde_json::to_value(db::JobConfiguration::from_records(
        &model_record,
        &prompt_record,
        analysis_profile,
    ))?;
    let job_id = Uuid::new_v4();
    let timeout_at = OffsetDateTime::now_utc()
        + time::Duration::seconds(state.config.job_timeout.as_secs() as i64);
    let result = sqlx::query_as::<_, AnalysisResponse>(
        r#"
        INSERT INTO analysis_jobs(
            id, user_id, document_id, model_profile_id, analysis_profile, status,
            prompt_version_id, model_snapshot, idempotency_key, analyzer_version, timeout_at
        ) VALUES($1,$2,$3,$4,$5,'queued',$6,$7,$8,$9,$10)
        RETURNING id, document_id, model_profile_id, analysis_profile, status, stage, progress,
                  total_chunks, completed_chunks, error_code, error_message,
                  report_id, created_at, started_at, completed_at
        "#,
    )
    .bind(job_id)
    .bind(user.id)
    .bind(input.document_id)
    .bind(input.model_profile_id)
    .bind(analysis_profile.as_str())
    .bind(prompt_record.id)
    .bind(model_snapshot)
    .bind(idempotency_key.as_deref())
    .bind(ANALYZER_VERSION)
    .bind(timeout_at)
    .fetch_one(&state.pool)
    .await;
    match result {
        Ok(job) => Ok((StatusCode::ACCEPTED, Json(job))),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            let key = idempotency_key.as_deref().ok_or_else(|| {
                ApiError(CoreError::public(ErrorCode::Conflict, "分析任务已存在"))
            })?;
            let job = fetch_by_idempotency(&state, user.id, key)
                .await?
                .ok_or_else(|| {
                    ApiError(CoreError::public(ErrorCode::Conflict, "分析任务幂等冲突"))
                })?;
            ensure_same_idempotent_request(&job, &input)?;
            Ok((StatusCode::OK, Json(job)))
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_same_idempotent_request(
    existing: &AnalysisResponse,
    input: &CreateAnalysisRequest,
) -> ApiResult<()> {
    if existing.document_id != input.document_id
        || existing.model_profile_id != input.model_profile_id
        || existing.analysis_profile != input.analysis_profile.unwrap_or_default().as_str()
    {
        return Err(ApiError(CoreError::public(
            ErrorCode::Conflict,
            "同一 Idempotency-Key 已用于不同的分析请求",
        )));
    }
    Ok(())
}

async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(job_id): Path<Uuid>,
) -> ApiResult<Json<AnalysisResponse>> {
    Ok(Json(fetch_job(&state, user.id, job_id).await?))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(job_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT analysis_jobs.document_id, analysis_jobs.status, reports.pdf_path
        FROM analysis_jobs
        LEFT JOIN reports ON reports.id = analysis_jobs.report_id
        WHERE analysis_jobs.id = $1 AND analysis_jobs.user_id = $2
        "#,
    )
    .bind(job_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((document_id, status, report_path)) = row else {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "分析任务不存在",
        )));
    };
    if matches!(
        status.as_str(),
        "queued"
            | "extracting"
            | "analyzing"
            | "verifying"
            | "merging"
            | "rendering"
            | "cancel_requested"
    ) {
        return Err(ApiError(CoreError::public(
            ErrorCode::Conflict,
            "请先取消任务并等待取消完成，再执行删除",
        )));
    }

    let jobs_for_document: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analysis_jobs WHERE document_id = $1")
            .bind(document_id)
            .fetch_one(&state.pool)
            .await?;
    let document_path = if jobs_for_document == 1 {
        let path: Option<String> =
            sqlx::query_scalar("SELECT storage_path FROM documents WHERE id = $1 AND user_id = $2")
                .bind(document_id)
                .bind(user.id)
                .fetch_optional(&state.pool)
                .await?
                .flatten();
        path
    } else {
        None
    };
    for path in [report_path.as_ref(), document_path.as_ref()]
        .into_iter()
        .flatten()
    {
        state.storage.remove(std::path::Path::new(path)).await?;
    }

    let mut transaction = state.pool.begin().await?;
    sqlx::query("DELETE FROM analysis_jobs WHERE id = $1 AND user_id = $2")
        .bind(job_id)
        .bind(user.id)
        .execute(&mut *transaction)
        .await?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analysis_jobs WHERE document_id = $1")
            .bind(document_id)
            .fetch_one(&mut *transaction)
            .await?;
    if remaining == 0 {
        sqlx::query("DELETE FROM documents WHERE id = $1 AND user_id = $2")
            .bind(document_id)
            .bind(user.id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(job_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let outcome = textcomb_core::db::request_cancellation(&state.pool, job_id, user.id).await?;
    if let textcomb_core::db::CancellationOutcome::Finalized(Some(path)) = outcome {
        match state.storage.remove(&path).await {
            Ok(()) => {
                if let Err(error) =
                    textcomb_core::db::confirm_document_file_removed(&state.pool, &path).await
                {
                    warn!(job_id = %job_id, error = %error, "cancelled input was removed but its cleanup marker remains");
                }
            }
            Err(error) => {
                warn!(job_id = %job_id, error = %error, "failed to remove queued cancellation input; cleanup will retry");
            }
        }
    }
    Ok(StatusCode::ACCEPTED)
}

async fn retry(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<AnalysisResponse>)> {
    let idempotency_key = parse_idempotency_key(&headers)?;
    let timeout_at = OffsetDateTime::now_utc()
        + time::Duration::seconds(state.config.job_timeout.as_secs() as i64);
    let result = sqlx::query(
        r#"
        UPDATE analysis_jobs AS job
        SET status = 'queued', stage = 'queued', progress = 0,
            completed_chunks = 0,
            error_code = NULL, error_message = NULL, worker_id = NULL,
            lease_until = NULL, heartbeat_at = NULL, timeout_at = $3,
            completed_at = NULL
        FROM documents
        WHERE job.id = $1 AND job.user_id = $2 AND job.status = 'failed'
          AND documents.id = job.document_id
          AND documents.storage_path IS NOT NULL
          AND documents.input_expires_at > now()
        "#,
    )
    .bind(job_id)
    .bind(user.id)
    .bind(timeout_at)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() != 1 {
        if idempotency_key.is_some() {
            let current = fetch_job(&state, user.id, job_id).await?;
            if current.status != "failed" {
                return Ok((StatusCode::OK, Json(current)));
            }
        }
        return Err(ApiError(CoreError::public(
            ErrorCode::Conflict,
            "任务不能重试；原文件可能已过期，请重新上传",
        )));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(fetch_job(&state, user.id, job_id).await?),
    ))
}

fn parse_idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if key.as_ref().is_some_and(|value| {
        let length = value.chars().count();
        !(8..=128).contains(&length)
    }) {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "Idempotency-Key 长度必须为 8–128 个字符",
        )));
    }
    Ok(key)
}

async fn events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(job_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    fetch_job(&state, user.id, job_id).await?;
    let pool = state.pool.clone();
    let stream = stream! {
        let mut last_signature = String::new();
        loop {
            let job = sqlx::query_as::<_, AnalysisResponse>(
                r#"
                SELECT id, document_id, model_profile_id, analysis_profile, status, stage, progress,
                       total_chunks, completed_chunks, error_code, error_message,
                       report_id, created_at, started_at, completed_at
                FROM analysis_jobs WHERE id = $1 AND user_id = $2
                "#,
            )
            .bind(job_id)
            .bind(user.id)
            .fetch_optional(&pool)
            .await;
            match job {
                Ok(Some(job)) => {
                    let signature = format!("{}:{}:{}:{}", job.status, job.stage, job.progress, job.completed_chunks);
                    if signature != last_signature {
                        last_signature = signature;
                        let data = serde_json::to_string(&job).unwrap_or_else(|_| "{}".to_owned());
                        yield Ok::<Event, Infallible>(Event::default().event("progress").data(data));
                    }
                    if matches!(job.status.as_str(), "completed" | "failed" | "cancelled" | "expired") {
                        break;
                    }
                }
                _ => break,
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

async fn fetch_job(state: &AppState, user_id: Uuid, job_id: Uuid) -> ApiResult<AnalysisResponse> {
    sqlx::query_as::<_, AnalysisResponse>(
        r#"
        SELECT id, document_id, model_profile_id, analysis_profile, status, stage, progress,
               total_chunks, completed_chunks, error_code, error_message,
               report_id, created_at, started_at, completed_at
        FROM analysis_jobs WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(job_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError(CoreError::public(ErrorCode::NotFound, "分析任务不存在")))
}

async fn fetch_by_idempotency(
    state: &AppState,
    user_id: Uuid,
    key: &str,
) -> ApiResult<Option<AnalysisResponse>> {
    Ok(sqlx::query_as::<_, AnalysisResponse>(
        r#"
        SELECT id, document_id, model_profile_id, analysis_profile, status, stage, progress,
               total_chunks, completed_chunks, error_code, error_message,
               report_id, created_at, started_at, completed_at
        FROM analysis_jobs WHERE user_id = $1 AND idempotency_key = $2
        "#,
    )
    .bind(user_id)
    .bind(key)
    .fetch_optional(&state.pool)
    .await?)
}
