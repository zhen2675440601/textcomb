use crate::{
    problem::{ApiError, ApiResult},
    session::AuthUser,
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
    crypto::{decrypt_secret, encrypt_secret},
    db, prompts,
    provider::{AnalysisProvider, OpenAiCompatibleProvider, ProviderProfile},
};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/model-profiles", get(list).post(create))
        .route("/model-profiles/{id}/test", post(test))
        .route("/model-profiles/{id}/enabled", patch(set_enabled))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ModelProfileResponse {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub base_url: String,
    pub candidate_model: String,
    pub verifier_model: String,
    pub max_concurrency: i32,
    pub enabled: bool,
    pub is_reference: bool,
    pub shared: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<ModelProfileResponse>>> {
    let profiles = sqlx::query_as::<_, ModelProfileResponse>(
        r#"
        SELECT id, name, provider_kind, base_url, candidate_model, verifier_model,
               max_concurrency, enabled, is_reference, owner_id IS NULL AS shared, created_at
        FROM model_profiles
        WHERE owner_id IS NULL OR owner_id = $1
        ORDER BY shared DESC, created_at
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(profiles))
}

#[derive(Debug, Deserialize)]
struct CreateModelProfileRequest {
    name: String,
    base_url: String,
    api_key: String,
    candidate_model: String,
    verifier_model: Option<String>,
    max_concurrency: Option<i32>,
    shared: Option<bool>,
    disclosure_accepted: bool,
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateModelProfileRequest>,
) -> ApiResult<(StatusCode, Json<ModelProfileResponse>)> {
    if !input.disclosure_accepted {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "必须确认正文将发送给第三方模型服务",
        )));
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型配置名称长度必须为 1–100 个字符",
        )));
    }
    if input.api_key.is_empty() || input.api_key.chars().count() > 16_384 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型密钥长度必须为 1–16384 个字符",
        )));
    }
    if input.base_url.chars().count() > 2_048 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型服务地址不能超过 2048 个字符",
        )));
    }
    let base_url = validate_base_url(&input.base_url)?;
    let shared = input.shared.unwrap_or(false);
    if shared && !user.is_admin() {
        return Err(ApiError(CoreError::public(
            ErrorCode::Forbidden,
            "仅超管可以创建共享模型配置",
        )));
    }
    let candidate_model = input.candidate_model.trim();
    if candidate_model.is_empty() || candidate_model.chars().count() > 160 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "候选模型名称长度必须为 1–160 个字符",
        )));
    }
    let verifier_model = input
        .verifier_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(candidate_model);
    if verifier_model.chars().count() > 160 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "复核模型名称不能超过 160 个字符",
        )));
    }
    let encrypted = encrypt_secret(&state.config.master_key, &SecretString::from(input.api_key))?;
    let id = Uuid::new_v4();
    let owner_id = (!shared).then_some(user.id);
    let profile = sqlx::query_as::<_, ModelProfileResponse>(
        r#"
        INSERT INTO model_profiles(
            id, owner_id, name, base_url, api_key_ciphertext,
            candidate_model, verifier_model, max_concurrency,
            disclosure_accepted_at
        )
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,now())
        RETURNING id, name, provider_kind, base_url, candidate_model, verifier_model,
                  max_concurrency, enabled, is_reference, owner_id IS NULL AS shared, created_at
        "#,
    )
    .bind(id)
    .bind(owner_id)
    .bind(name)
    .bind(base_url.as_str().trim_end_matches('/'))
    .bind(encrypted)
    .bind(candidate_model)
    .bind(verifier_model)
    .bind(input.max_concurrency.unwrap_or(10).clamp(1, 100))
    .fetch_one(&state.pool)
    .await?;
    Ok((StatusCode::CREATED, Json(profile)))
}

async fn test(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(profile_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let record = db::load_model_profile(&state.pool, profile_id, user.id).await?;
    let api_key = decrypt_secret(&state.config.master_key, &record.api_key_ciphertext)?;
    let provider = OpenAiCompatibleProvider::new(ProviderProfile {
        base_url: record.base_url,
        api_key,
        candidate_model: record.candidate_model,
        verifier_model: record.verifier_model,
        candidate_system_prompt: prompts::CANDIDATE_SYSTEM_PROMPT.to_owned(),
        verifier_system_prompt: prompts::VERIFIER_SYSTEM_PROMPT.to_owned(),
    })?;
    provider.test_connection().await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct SetEnabledRequest {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(profile_id): Path<Uuid>,
    Json(input): Json<SetEnabledRequest>,
) -> ApiResult<StatusCode> {
    let result = sqlx::query(
        r#"
        UPDATE model_profiles SET enabled = $3, updated_at = now()
        WHERE id = $1 AND (owner_id = $2 OR (owner_id IS NULL AND EXISTS(
            SELECT 1 FROM users WHERE id = $2 AND role = 'super_admin'
        )))
        "#,
    )
    .bind(profile_id)
    .bind(user.id)
    .bind(input.enabled)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "模型配置不存在",
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn validate_base_url(input: &str) -> Result<Url, ApiError> {
    let url = Url::parse(input).map_err(|_| {
        ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型服务地址不是有效 URL",
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型服务地址只能使用 http/https，且不得包含凭据、查询参数或片段",
        )));
    }
    Ok(url)
}
