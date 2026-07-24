use crate::{problem::ApiError, state::AppState};
use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{header, request::Parts},
};
use textcomb_core::{
    CoreError, ErrorCode,
    auth::{self, UserIdentity},
};

pub const SESSION_COOKIE: &str = "textcomb_session";

#[derive(Debug, Clone)]
pub struct AuthUser(pub UserIdentity);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = cookie_value(parts, SESSION_COOKIE)
            .ok_or_else(|| ApiError(CoreError::public(ErrorCode::Unauthorized, "请先登录")))?;
        Ok(Self(auth::authenticate(&state.pool, token).await?))
    }
}

impl OptionalFromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        let Some(token) = cookie_value(parts, SESSION_COOKIE) else {
            return Ok(None);
        };
        match auth::authenticate(&state.pool, token).await {
            Ok(user) => Ok(Some(Self(user))),
            Err(error) if error.code() == ErrorCode::Unauthorized => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdminUser(pub UserIdentity);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AuthUser(user) =
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(parts, state).await?;
        if !user.is_admin() {
            return Err(ApiError(CoreError::public(
                ErrorCode::Forbidden,
                "仅超管可以执行此操作",
            )));
        }
        Ok(Self(user))
    }
}

pub fn cookie_value<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|value| value.trim().split_once('='))
        .find_map(|(cookie_name, value)| (cookie_name == name).then_some(value))
}
