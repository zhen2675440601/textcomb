use crate::{
    api::auth::UserResponse,
    problem::{ApiError, ApiResult},
    session::AdminUser,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use textcomb_core::{
    CoreError, ErrorCode,
    auth::{self, UserIdentity},
};
use time::OffsetDateTime;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/system-status", get(system_status))
        .route("/admin/users", get(list_users).post(create_user))
        .route("/admin/users/{id}/status", patch(set_status))
        .route("/admin/users/{id}/password", post(reset_password))
}

#[derive(Debug, Serialize)]
struct SystemStatusResponse {
    database: &'static str,
    queued_jobs: i64,
    active_jobs: i64,
    active_workers: i64,
    failed_jobs_last_24h: i64,
    retained_reports: i64,
    #[serde(with = "time::serde::rfc3339")]
    server_time: OffsetDateTime,
}

async fn system_status(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> ApiResult<Json<SystemStatusResponse>> {
    let (queued_jobs, active_jobs, active_workers, failed_jobs_last_24h): (i64, i64, i64, i64) =
        sqlx::query_as(
            r#"
        SELECT
            count(*) FILTER (WHERE status = 'queued'),
            count(*) FILTER (
                WHERE status IN (
                    'extracting', 'analyzing', 'verifying', 'merging', 'rendering',
                    'cancel_requested'
                )
            ),
            count(DISTINCT worker_id) FILTER (
                WHERE worker_id IS NOT NULL AND lease_until > now()
            ),
            count(*) FILTER (
                WHERE status = 'failed' AND completed_at >= now() - interval '24 hours'
            )
        FROM analysis_jobs
        "#,
        )
        .fetch_one(&state.pool)
        .await?;
    let retained_reports: i64 =
        sqlx::query_scalar("SELECT count(*) FROM reports WHERE expires_at > now()")
            .fetch_one(&state.pool)
            .await?;

    Ok(Json(SystemStatusResponse {
        database: "ready",
        queued_jobs,
        active_jobs,
        active_workers,
        failed_jobs_last_24h,
        retained_reports,
        server_time: OffsetDateTime::now_utc(),
    }))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct AdminUserResponse {
    id: Uuid,
    username: String,
    role: String,
    status: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

async fn list_users(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> ApiResult<Json<Vec<AdminUserResponse>>> {
    let users = sqlx::query_as::<_, AdminUserResponse>(
        "SELECT id, username, role, status, created_at FROM users ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(users))
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
}

async fn create_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Json(input): Json<CreateUserRequest>,
) -> ApiResult<(StatusCode, Json<UserResponse>)> {
    let user = auth::create_user(
        &state.pool,
        input.username.trim(),
        &SecretString::from(input.password),
        "user",
    )
    .await?;
    audit(&state, &admin, "user.create", "user", Some(user.id)).await?;
    Ok((StatusCode::CREATED, Json(UserResponse::from(user))))
}

#[derive(Debug, Deserialize)]
struct SetStatusRequest {
    enabled: bool,
}

async fn set_status(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(input): Json<SetStatusRequest>,
) -> ApiResult<StatusCode> {
    if admin.id == user_id && !input.enabled {
        return Err(ApiError(CoreError::public(
            ErrorCode::Conflict,
            "不能停用当前登录的超管账号",
        )));
    }
    auth::set_user_enabled(&state.pool, user_id, input.enabled).await?;
    audit(
        &state,
        &admin,
        if input.enabled {
            "user.enable"
        } else {
            "user.disable"
        },
        "user",
        Some(user_id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ResetPasswordRequest {
    password: String,
}

async fn reset_password(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(input): Json<ResetPasswordRequest>,
) -> ApiResult<StatusCode> {
    auth::reset_password(&state.pool, user_id, &SecretString::from(input.password)).await?;
    audit(&state, &admin, "user.reset_password", "user", Some(user_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn audit(
    state: &AppState,
    actor: &UserIdentity,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit_events(actor_user_id, action, target_type, target_id) VALUES($1,$2,$3,$4)",
    )
    .bind(actor.id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}
