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
    provider::{ProviderKind, ProviderProfile, create_provider},
};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/model-profiles", get(list).post(create))
        .route("/model-profiles/{id}", patch(update))
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
struct ModelProfileFields {
    name: String,
    #[serde(default = "default_provider_kind")]
    provider_kind: String,
    base_url: String,
    candidate_model: String,
    verifier_model: Option<String>,
    max_concurrency: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CreateModelProfileRequest {
    #[serde(flatten)]
    fields: ModelProfileFields,
    api_key: String,
    shared: Option<bool>,
    disclosure_accepted: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateModelProfileRequest {
    #[serde(flatten)]
    fields: ModelProfileFields,
    /// Omitting the key (or submitting an empty string) preserves the encrypted value.
    api_key: Option<String>,
    disclosure_accepted: bool,
}

#[derive(Debug)]
struct ValidatedModelProfile {
    name: String,
    provider_kind: ProviderKind,
    base_url: String,
    candidate_model: String,
    verifier_model: String,
    max_concurrency: i32,
}

fn default_provider_kind() -> String {
    ProviderKind::default().as_str().to_owned()
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateModelProfileRequest>,
) -> ApiResult<(StatusCode, Json<ModelProfileResponse>)> {
    validate_disclosure(input.disclosure_accepted)?;
    validate_api_key(&input.api_key)?;
    let fields = validate_model_profile_fields(&input.fields)?;
    let shared = input.shared.unwrap_or(false);
    if shared && !user.is_admin() {
        return Err(ApiError(CoreError::public(
            ErrorCode::Forbidden,
            "仅超管可以创建共享模型配置",
        )));
    }
    let encrypted = encrypt_secret(&state.config.master_key, &SecretString::from(input.api_key))?;
    let id = Uuid::new_v4();
    let owner_id = (!shared).then_some(user.id);
    let profile = sqlx::query_as::<_, ModelProfileResponse>(
        r#"
        INSERT INTO model_profiles(
            id, owner_id, name, provider_kind, base_url, api_key_ciphertext,
            candidate_model, verifier_model, max_concurrency,
            disclosure_accepted_at
        )
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,now())
        RETURNING id, name, provider_kind, base_url, candidate_model, verifier_model,
                  max_concurrency, enabled, is_reference, owner_id IS NULL AS shared, created_at
        "#,
    )
    .bind(id)
    .bind(owner_id)
    .bind(fields.name)
    .bind(fields.provider_kind.as_str())
    .bind(fields.base_url)
    .bind(encrypted)
    .bind(fields.candidate_model)
    .bind(fields.verifier_model)
    .bind(fields.max_concurrency)
    .fetch_one(&state.pool)
    .await?;
    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(profile_id): Path<Uuid>,
    Json(input): Json<UpdateModelProfileRequest>,
) -> ApiResult<Json<ModelProfileResponse>> {
    validate_disclosure(input.disclosure_accepted)?;
    let fields = validate_model_profile_fields(&input.fields)?;
    let api_key_ciphertext = match input.api_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => {
            validate_api_key(key)?;
            Some(encrypt_secret(
                &state.config.master_key,
                &SecretString::from(key.to_owned()),
            )?)
        }
        None => None,
    };

    let profile = sqlx::query_as::<_, ModelProfileResponse>(
        r#"
        UPDATE model_profiles
        SET name = $3,
            provider_kind = $4,
            base_url = $5,
            api_key_ciphertext = COALESCE($6, api_key_ciphertext),
            candidate_model = $7,
            verifier_model = $8,
            max_concurrency = $9,
            disclosure_accepted_at = now(),
            updated_at = now()
        WHERE id = $1 AND (owner_id = $2 OR (owner_id IS NULL AND EXISTS(
            SELECT 1 FROM users WHERE id = $2 AND role = 'super_admin'
        )))
        RETURNING id, name, provider_kind, base_url, candidate_model, verifier_model,
                  max_concurrency, enabled, is_reference, owner_id IS NULL AS shared, created_at
        "#,
    )
    .bind(profile_id)
    .bind(user.id)
    .bind(fields.name)
    .bind(fields.provider_kind.as_str())
    .bind(fields.base_url)
    .bind(api_key_ciphertext)
    .bind(fields.candidate_model)
    .bind(fields.verifier_model)
    .bind(fields.max_concurrency)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError(CoreError::public(ErrorCode::NotFound, "模型配置不存在")))?;

    Ok(Json(profile))
}

async fn test(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(profile_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let record = db::load_model_profile(&state.pool, profile_id, user.id).await?;
    let api_key = decrypt_secret(&state.config.master_key, &record.api_key_ciphertext)?;
    let provider_kind = ProviderKind::parse(&record.provider_kind).map_err(ApiError)?;
    let provider = create_provider(
        provider_kind,
        ProviderProfile {
            base_url: record.base_url,
            api_key,
            candidate_model: record.candidate_model,
            verifier_model: record.verifier_model,
            candidate_system_prompt: prompts::CANDIDATE_SYSTEM_PROMPT.to_owned(),
            verifier_system_prompt: prompts::VERIFIER_SYSTEM_PROMPT.to_owned(),
        },
    )?;
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

fn validate_disclosure(accepted: bool) -> Result<(), ApiError> {
    if accepted {
        return Ok(());
    }
    Err(ApiError(CoreError::public(
        ErrorCode::ConfigurationInvalid,
        "必须确认正文将发送给第三方模型服务",
    )))
}

fn validate_api_key(api_key: &str) -> Result<(), ApiError> {
    if api_key.is_empty() || api_key.chars().count() > 16_384 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型密钥长度必须为 1–16384 个字符",
        )));
    }
    Ok(())
}

fn validate_model_profile_fields(
    input: &ModelProfileFields,
) -> Result<ValidatedModelProfile, ApiError> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型配置名称长度必须为 1–100 个字符",
        )));
    }
    if input.base_url.chars().count() > 2_048 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "模型服务地址不能超过 2048 个字符",
        )));
    }
    let provider_kind = ProviderKind::parse(&input.provider_kind).map_err(ApiError)?;
    let base_url = validate_base_url(&input.base_url)?;
    let candidate_model = input.candidate_model.trim();
    if candidate_model.is_empty() || candidate_model.chars().count() > 160 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "初检模型名称长度必须为 1–160 个字符",
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

    Ok(ValidatedModelProfile {
        name: name.to_owned(),
        provider_kind,
        base_url: base_url.as_str().trim_end_matches('/').to_owned(),
        candidate_model: candidate_model.to_owned(),
        verifier_model: verifier_model.to_owned(),
        max_concurrency: input.max_concurrency.unwrap_or(10).clamp(1, 100),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_fields() -> ModelProfileFields {
        ModelProfileFields {
            name: "我的模型".to_owned(),
            provider_kind: "anthropic".to_owned(),
            base_url: "https://example.test/apps/anthropic/v1/".to_owned(),
            candidate_model: "candidate".to_owned(),
            verifier_model: Some("  ".to_owned()),
            max_concurrency: Some(200),
        }
    }

    #[test]
    fn profile_validation_normalizes_optional_fields() {
        let profile = validate_model_profile_fields(&valid_fields()).expect("valid profile");

        assert_eq!(profile.base_url, "https://example.test/apps/anthropic/v1");
        assert_eq!(profile.verifier_model, "candidate");
        assert_eq!(profile.max_concurrency, 100);
    }

    #[test]
    fn profile_validation_rejects_unsafe_url() {
        let mut fields = valid_fields();
        fields.base_url = "https://example.test/v1?api_key=secret".to_owned();

        assert!(validate_model_profile_fields(&fields).is_err());
    }

    #[test]
    fn key_validation_rejects_empty_replacement_key() {
        assert!(validate_api_key("").is_err());
        assert!(validate_api_key("new-key").is_ok());
    }
}
