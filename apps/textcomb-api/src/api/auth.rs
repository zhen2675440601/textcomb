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
use std::net::{IpAddr, SocketAddr};
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
        Some(client_ip(address, &headers)),
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

fn client_ip(address: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let trusted_proxy = match address.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unicast_link_local(),
    };
    if trusted_proxy
        && let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.trim().parse().ok())
    {
        return forwarded;
    }
    address.ip()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn login_rate_limit_uses_forwarded_client_ip_behind_private_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.8, 10.0.0.2"),
        );
        let address = SocketAddr::from(([10, 0, 0, 2], 8080));
        assert_eq!(
            client_ip(address, &headers),
            "203.0.113.8".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn direct_public_connection_cannot_spoof_forwarded_client_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.8"));
        let address = SocketAddr::from(([198, 51, 100, 4], 8080));
        assert_eq!(client_ip(address, &headers), address.ip());
    }
}
