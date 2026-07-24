use crate::{
    problem::ApiResult,
    session::{AuthUser, SESSION_COOKIE},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use textcomb_core::auth;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: uuid::Uuid,
    pub username: String,
    pub role: String,
}

impl From<textcomb_core::auth::UserIdentity> for UserResponse {
    fn from(user: textcomb_core::auth::UserIdentity) -> Self {
        Self {
            id: user.id,
            username: user.username,
            role: user.role,
        }
    }
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<LoginRequest>,
) -> ApiResult<Response> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let (user, token, _expires_at) = auth::login(
        &state.pool,
        input.username.trim(),
        &SecretString::from(input.password),
        Some(address.ip()),
        user_agent,
        state.config.session_ttl,
    )
    .await?;
    state
        .metrics
        .operations
        .with_label_values(&["login", "success"])
        .inc();
    let max_age = state.config.session_ttl.as_secs();
    let secure = if state.config.secure_cookies {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    );
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(UserResponse::from(user)),
    )
        .into_response())
}

pub async fn logout(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let _ = user;
    if let Some(raw_cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        let token = raw_cookie
            .split(';')
            .filter_map(|value| value.trim().split_once('='))
            .find_map(|(name, value)| (name == SESSION_COOKIE).then_some(value));
        if let Some(token) = token {
            auth::logout(&state.pool, token).await?;
        }
    }
    let secure = if state.config.secure_cookies {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}");
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response())
}

pub async fn me(AuthUser(user): AuthUser) -> Json<UserResponse> {
    Json(UserResponse::from(user))
}
