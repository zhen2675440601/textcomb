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
    // Lock the document before checking for another job. Concurrent create requests
    // then observe the first committed job instead of sharing one input lifecycle.
    let mut transaction = state.pool.begin().await?;
    let document_exists: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM documents
        WHERE id = $1 AND user_id = $2 AND storage_path IS NOT NULL
          AND input_expires_at > now()
        FOR UPDATE
        "#,
    )
    .bind(input.document_id)
    .bind(user.id)
    .fetch_optional(&mut *transaction)
    .await?;
    if document_exists.is_none() {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "文档不存在、已删除或已完成过分析",
        )));
    }
    let already_analyzed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM analysis_jobs WHERE document_id = $1)")
            .bind(input.document_id)
            .fetch_one(&mut *transaction)
            .await?;
    if already_analyzed {
        drop(transaction);
        if let Some(key) = idempotency_key.as_ref()
            && let Some(existing) = fetch_by_idempotency(&state, user.id, key).await?
        {
            ensure_same_idempotent_request(&existing, &input)?;
            return Ok((StatusCode::OK, Json(existing)));
        }
        return Err(ApiError(CoreError::public(
            ErrorCode::Conflict,
            "该文档已有分析任务；失败任务请使用重试接口",
        )));
    }
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
    .fetch_one(&mut *transaction)
    .await;
    match result {
        Ok(job) => {
            transaction.commit().await?;
            Ok((StatusCode::ACCEPTED, Json(job)))
        }
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            drop(transaction);
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
    super::deletion::remove(&state, user.id, super::deletion::Target::Analysis(job_id)).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiMetrics;
    use std::sync::Arc;
    use textcomb_core::{AppConfig, auth::UserIdentity, storage::LocalStorage};

    struct DeletionFixture {
        state: AppState,
        user: AuthUser,
        job_id: Uuid,
        document_id: Uuid,
        profile_id: Uuid,
        input_path: std::path::PathBuf,
        application_name: String,
    }

    impl DeletionFixture {
        async fn create(url: &str) -> Self {
            use std::str::FromStr;
            let user_id = Uuid::new_v4();
            let job_id = Uuid::new_v4();
            let document_id = Uuid::new_v4();
            let profile_id = Uuid::new_v4();
            let application_name = format!("deletion-{job_id}");
            let options = sqlx::postgres::PgConnectOptions::from_str(url)
                .unwrap()
                .application_name(&application_name);
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(6)
                .connect_with(options)
                .await
                .unwrap();
            let root = std::env::temp_dir().join(format!("textcomb-deletion-{job_id}"));
            let state = AppState {
                pool: pool.clone(),
                config: Arc::new(AppConfig::from_env().unwrap()),
                storage: LocalStorage::new(root).await.unwrap(),
                metrics: ApiMetrics::new().unwrap(),
            };
            let user = AuthUser(UserIdentity {
                id: user_id,
                username: format!("deletion-{user_id}"),
                role: "user".to_owned(),
            });
            let input_path = state
                .storage
                .write_upload(document_id, "txt", "测试正文".as_bytes())
                .await
                .unwrap();
            sqlx::query("INSERT INTO users(id, username, password_hash, role) VALUES($1,$2,'unused','user')")
                .bind(user_id).bind(&user.0.username).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO model_profiles(id, owner_id, name, base_url, api_key_ciphertext, candidate_model, verifier_model, disclosure_accepted_at) VALUES($1,$2,'test','https://example.test',decode('00','hex'),'test','test',now())")
                .bind(profile_id).bind(user_id).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO documents(id, user_id, original_name, media_type, document_format, size_bytes, storage_path, input_expires_at) VALUES($1,$2,'test.txt','text/plain','txt',12,$3,now() + interval '1 hour')")
                .bind(document_id).bind(user_id).bind(input_path.to_string_lossy().as_ref()).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO analysis_jobs(id, user_id, document_id, model_profile_id, status, analyzer_version, timeout_at) VALUES($1,$2,$3,$4,'failed','test',now() + interval '1 hour')")
                .bind(job_id).bind(user_id).bind(document_id).bind(profile_id).execute(&pool).await.unwrap();
            Self {
                state,
                user,
                job_id,
                document_id,
                profile_id,
                input_path,
                application_name,
            }
        }

        async fn wait_for_lock_waiters(&self, minimum: i64) {
            // Observe actual PostgreSQL lock waits instead of relying on task scheduling.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let waiters: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1 AND wait_event_type = 'Lock'")
                        .bind(&self.application_name).fetch_one(&self.state.pool).await.unwrap();
                    if waiters >= minimum { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("API requests did not reach their row lock");
        }

        async fn cleanup(&self) {
            sqlx::query("DELETE FROM analysis_jobs WHERE user_id = $1")
                .bind(self.user.0.id)
                .execute(&self.state.pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(self.user.0.id)
                .execute(&self.state.pool)
                .await
                .unwrap();
            tokio::fs::remove_dir_all(self.state.storage.root())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn deletion_document_rejects_all_active_stages_and_other_owners() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let fixture = DeletionFixture::create(&url).await;
        for status in [
            "queued",
            "extracting",
            "analyzing",
            "verifying",
            "merging",
            "rendering",
            "cancel_requested",
        ] {
            sqlx::query("UPDATE analysis_jobs SET status = $2 WHERE id = $1")
                .bind(fixture.job_id)
                .bind(status)
                .execute(&fixture.state.pool)
                .await
                .unwrap();
            let error = super::super::documents::remove(
                State(fixture.state.clone()),
                fixture.user.clone(),
                Path(fixture.document_id),
            )
            .await
            .unwrap_err();
            assert_eq!(error.0.code(), ErrorCode::Conflict, "stage {status}");
            assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
            assert_eq!(
                fetch_job(&fixture.state, fixture.user.0.id, fixture.job_id)
                    .await
                    .unwrap()
                    .status,
                status
            );
        }
        let outsider = AuthUser(UserIdentity {
            id: Uuid::new_v4(),
            username: "outsider".to_owned(),
            role: "user".to_owned(),
        });
        let error = super::super::documents::remove(
            State(fixture.state.clone()),
            outsider,
            Path(fixture.document_id),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0.code(), ErrorCode::NotFound);
        assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
        fixture.cleanup().await;
    }

    #[tokio::test]
    async fn deletion_document_preserves_input_when_creation_wins_the_document_lock() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let fixture = DeletionFixture::create(&url).await;
        sqlx::query("DELETE FROM analysis_jobs WHERE id = $1")
            .bind(fixture.job_id)
            .execute(&fixture.state.pool)
            .await
            .unwrap();
        let mut gate = fixture.state.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM documents WHERE id = $1 FOR UPDATE")
            .bind(fixture.document_id)
            .fetch_one(&mut *gate)
            .await
            .unwrap();
        let creation = {
            let state = fixture.state.clone();
            let user = fixture.user.clone();
            let input = CreateAnalysisRequest {
                document_id: fixture.document_id,
                model_profile_id: fixture.profile_id,
                analysis_profile: Some(AnalysisProfile::General),
            };
            tokio::spawn(
                async move { create(State(state), user, HeaderMap::new(), Json(input)).await },
            )
        };
        fixture.wait_for_lock_waiters(1).await;
        let deletion = {
            let state = fixture.state.clone();
            let user = fixture.user.clone();
            let document_id = fixture.document_id;
            tokio::spawn(async move {
                super::super::documents::remove(State(state), user, Path(document_id)).await
            })
        };
        fixture.wait_for_lock_waiters(2).await;
        assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
        gate.commit().await.unwrap();
        let (status, Json(job)) = creation.await.unwrap().unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            deletion.await.unwrap().unwrap_err().0.code(),
            ErrorCode::Conflict
        );
        assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
        assert_eq!(
            fetch_job(&fixture.state, fixture.user.0.id, job.id)
                .await
                .unwrap()
                .status,
            "queued"
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    async fn deletion_report_checks_related_jobs_and_clears_source_only_when_safe() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let fixture = DeletionFixture::create(&url).await;
        let report_id = Uuid::new_v4();
        let related_job_id = Uuid::new_v4();
        let pool = &fixture.state.pool;
        sqlx::query("INSERT INTO reports(id, job_id, user_id, document_name, content_json, markdown, expires_at) VALUES($1,$2,$3,'test.txt','{}','test',now() + interval '1 day')")
            .bind(report_id).bind(fixture.job_id).bind(fixture.user.0.id).execute(pool).await.unwrap();
        sqlx::query("UPDATE analysis_jobs SET status = 'completed', report_id = $2 WHERE id = $1")
            .bind(fixture.job_id)
            .bind(report_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE documents SET extracted_text = '测试正文' WHERE id = $1")
            .bind(fixture.document_id)
            .execute(pool)
            .await
            .unwrap();
        // Legacy databases can contain multiple jobs sharing one document.
        sqlx::query("INSERT INTO analysis_jobs(id, user_id, document_id, model_profile_id, status, analyzer_version, timeout_at) VALUES($1,$2,$3,$4,'queued','test',now() + interval '1 hour')")
            .bind(related_job_id).bind(fixture.user.0.id).bind(fixture.document_id).bind(fixture.profile_id)
            .execute(pool).await.unwrap();
        let outsider = AuthUser(UserIdentity {
            id: Uuid::new_v4(),
            username: "outsider".to_owned(),
            role: "user".to_owned(),
        });
        assert_eq!(
            super::super::reports::delete_report(
                State(fixture.state.clone()),
                outsider,
                Path(report_id),
            )
            .await
            .unwrap_err()
            .0
            .code(),
            ErrorCode::NotFound
        );
        assert_eq!(
            super::super::reports::delete_report(
                State(fixture.state.clone()),
                fixture.user.clone(),
                Path(report_id),
            )
            .await
            .unwrap_err()
            .0
            .code(),
            ErrorCode::Conflict
        );
        assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
        let body: Option<String> =
            sqlx::query_scalar("SELECT extracted_text FROM documents WHERE id = $1")
                .bind(fixture.document_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(body.as_deref(), Some("测试正文"));
        sqlx::query("DELETE FROM analysis_jobs WHERE id = $1")
            .bind(related_job_id)
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(
            super::super::reports::delete_report(
                State(fixture.state.clone()),
                fixture.user.clone(),
                Path(report_id),
            )
            .await
            .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert!(!tokio::fs::try_exists(&fixture.input_path).await.unwrap());
        let source: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT extracted_text, storage_path FROM documents WHERE id = $1")
                .bind(fixture.document_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(source, (None, None));
        let job = fetch_job(&fixture.state, fixture.user.0.id, fixture.job_id)
            .await
            .unwrap();
        assert_eq!(job.status, "expired");
        assert!(job.report_id.is_none());
        let report_count: i64 = sqlx::query_scalar("SELECT count(*) FROM reports WHERE id = $1")
            .bind(report_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(report_count, 0);
        fixture.cleanup().await;
    }

    #[tokio::test]
    async fn deletion_last_shared_report_clears_body_without_deleting_other_job_metadata() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let fixture = DeletionFixture::create(&url).await;
        let pool = &fixture.state.pool;
        let second_job = Uuid::new_v4();
        sqlx::query("INSERT INTO analysis_jobs(id, user_id, document_id, model_profile_id, status, analyzer_version, timeout_at) VALUES($1,$2,$3,$4,'completed','test',now() + interval '1 hour')")
            .bind(second_job).bind(fixture.user.0.id).bind(fixture.document_id).bind(fixture.profile_id)
            .execute(pool).await.unwrap();
        sqlx::query("UPDATE documents SET extracted_text = '测试正文' WHERE id = $1")
            .bind(fixture.document_id)
            .execute(pool)
            .await
            .unwrap();
        let reports = [Uuid::new_v4(), Uuid::new_v4()];
        for (job, report) in [fixture.job_id, second_job].into_iter().zip(reports) {
            sqlx::query("INSERT INTO reports(id, job_id, user_id, document_name, content_json, markdown, expires_at) VALUES($1,$2,$3,'test.txt','{}','test',now() + interval '1 day')")
                .bind(report).bind(job).bind(fixture.user.0.id).execute(pool).await.unwrap();
            sqlx::query(
                "UPDATE analysis_jobs SET status = 'completed', report_id = $2 WHERE id = $1",
            )
            .bind(job)
            .bind(report)
            .execute(pool)
            .await
            .unwrap();
        }
        for (index, report) in reports.into_iter().enumerate() {
            assert_eq!(
                super::super::reports::delete_report(
                    State(fixture.state.clone()),
                    fixture.user.clone(),
                    Path(report),
                )
                .await
                .unwrap(),
                StatusCode::NO_CONTENT
            );
            let body: Option<String> =
                sqlx::query_scalar("SELECT extracted_text FROM documents WHERE id = $1")
                    .bind(fixture.document_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(body.is_some(), index == 0);
            assert_eq!(
                tokio::fs::try_exists(&fixture.input_path).await.unwrap(),
                index == 0
            );
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM analysis_jobs WHERE document_id = $1 AND status = 'expired'",
        )
        .bind(fixture.document_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(count, 2);
        fixture.cleanup().await;
    }

    #[tokio::test]
    async fn deletion_preserves_input_when_retry_wins_the_job_lock() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        for document_route in [false, true] {
            let fixture = DeletionFixture::create(&url).await;
            let mut gate = fixture.state.pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM analysis_jobs WHERE id = $1 FOR UPDATE")
                .bind(fixture.job_id)
                .fetch_one(&mut *gate)
                .await
                .unwrap();
            let retry_task = {
                let state = fixture.state.clone();
                let user = fixture.user.clone();
                let job_id = fixture.job_id;
                tokio::spawn(async move {
                    retry(State(state), user, Path(job_id), HeaderMap::new()).await
                })
            };
            fixture.wait_for_lock_waiters(1).await;
            let deletion_task = {
                let state = fixture.state.clone();
                let user = fixture.user.clone();
                let job_id = fixture.job_id;
                let document_id = fixture.document_id;
                tokio::spawn(async move {
                    if document_route {
                        super::super::documents::remove(State(state), user, Path(document_id)).await
                    } else {
                        remove(State(state), user, Path(job_id)).await
                    }
                })
            };
            fixture.wait_for_lock_waiters(2).await;
            assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
            gate.commit().await.unwrap();
            assert_eq!(retry_task.await.unwrap().unwrap().0, StatusCode::ACCEPTED);
            assert_eq!(
                deletion_task.await.unwrap().unwrap_err().0.code(),
                ErrorCode::Conflict
            );
            assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
            let job = fetch_job(&fixture.state, fixture.user.0.id, fixture.job_id)
                .await
                .unwrap();
            assert_eq!(job.status, "queued");
            fixture.cleanup().await;
        }
    }

    #[tokio::test]
    async fn deletion_prevents_requeue_when_deletion_wins_the_job_lock() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        for document_route in [false, true] {
            let fixture = DeletionFixture::create(&url).await;
            let mut gate = fixture.state.pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM analysis_jobs WHERE id = $1 FOR UPDATE")
                .bind(fixture.job_id)
                .fetch_one(&mut *gate)
                .await
                .unwrap();
            let deletion_task = {
                let state = fixture.state.clone();
                let user = fixture.user.clone();
                let job_id = fixture.job_id;
                let document_id = fixture.document_id;
                tokio::spawn(async move {
                    if document_route {
                        super::super::documents::remove(State(state), user, Path(document_id)).await
                    } else {
                        remove(State(state), user, Path(job_id)).await
                    }
                })
            };
            fixture.wait_for_lock_waiters(1).await;
            let retry_task = {
                let state = fixture.state.clone();
                let user = fixture.user.clone();
                let job_id = fixture.job_id;
                tokio::spawn(async move {
                    retry(State(state), user, Path(job_id), HeaderMap::new()).await
                })
            };
            fixture.wait_for_lock_waiters(2).await;
            assert!(tokio::fs::try_exists(&fixture.input_path).await.unwrap());
            gate.commit().await.unwrap();
            assert_eq!(
                deletion_task.await.unwrap().unwrap(),
                StatusCode::NO_CONTENT
            );
            assert_eq!(
                retry_task.await.unwrap().unwrap_err().0.code(),
                ErrorCode::Conflict
            );
            assert!(!tokio::fs::try_exists(&fixture.input_path).await.unwrap());
            assert_eq!(
                fetch_job(&fixture.state, fixture.user.0.id, fixture.job_id)
                    .await
                    .unwrap_err()
                    .0
                    .code(),
                ErrorCode::NotFound
            );
            fixture.cleanup().await;
        }
    }

    #[tokio::test]
    async fn concurrent_requests_create_only_one_job_for_a_document() {
        let Ok(database_url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
        let user_id = Uuid::new_v4();
        let document_id = Uuid::new_v4();
        let profile_id = Uuid::new_v4();
        let storage_root = std::env::temp_dir().join(format!("textcomb-job-test-{document_id}"));
        let state = AppState {
            pool: pool.clone(),
            config: Arc::new(AppConfig::from_env().unwrap()),
            storage: LocalStorage::new(&storage_root).await.unwrap(),
            metrics: ApiMetrics::new().unwrap(),
        };
        let user = AuthUser(UserIdentity {
            id: user_id,
            username: format!("job-{user_id}"),
            role: "user".to_owned(),
        });
        sqlx::query(
            "INSERT INTO users(id, username, password_hash, role) VALUES($1,$2,'unused','user')",
        )
        .bind(user_id)
        .bind(&user.0.username)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO model_profiles(id, owner_id, name, base_url, api_key_ciphertext, candidate_model, verifier_model, disclosure_accepted_at) VALUES($1,$2,'test','https://example.test',decode('00','hex'),'test','test',now())")
            .bind(profile_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO documents(id, user_id, original_name, media_type, document_format, size_bytes, storage_path, input_expires_at) VALUES($1,$2,'test.txt','text/plain','txt',4,$3,now() + interval '1 hour')")
            .bind(document_id)
            .bind(user_id)
            .bind(storage_root.join("test.txt").to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

        let input = || {
            Json(CreateAnalysisRequest {
                document_id,
                model_profile_id: profile_id,
                analysis_profile: Some(AnalysisProfile::Academic),
            })
        };
        let (first, second) = tokio::join!(
            create(
                State(state.clone()),
                user.clone(),
                HeaderMap::new(),
                input()
            ),
            create(State(state), user, HeaderMap::new(), input()),
        );
        assert_eq!(first.is_ok() as u8 + second.is_ok() as u8, 1);
        let conflict = first.err().or_else(|| second.err()).unwrap();
        assert_eq!(conflict.0.code(), ErrorCode::Conflict);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM analysis_jobs WHERE document_id = $1")
                .bind(document_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);

        let (analysis_profile, snapshot): (String, serde_json::Value) = sqlx::query_as(
            "SELECT analysis_profile, model_snapshot FROM analysis_jobs WHERE document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(analysis_profile, "academic");
        assert_eq!(snapshot["analysis_profile"], "academic");
        assert!(
            snapshot["candidate_system_prompt"]
                .as_str()
                .unwrap()
                .contains("当前场景：学术论文")
        );

        sqlx::query("DELETE FROM analysis_jobs WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        tokio::fs::remove_dir_all(storage_root).await.unwrap();
    }
}
