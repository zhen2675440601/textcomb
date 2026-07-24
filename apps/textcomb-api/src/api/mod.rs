mod admin;
mod analyses;
mod auth;
mod documents;
mod models;
mod reports;

use crate::{
    problem::{ApiError, ApiResult},
    state::AppState,
};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Response, StatusCode, header},
    response::IntoResponse,
};
use serde_json::json;
use textcomb_core::CoreError;

pub fn router() -> Router<AppState> {
    Router::new().nest(
        "/api/v1",
        Router::new()
            .merge(auth::router())
            .merge(admin::router())
            .merge(models::router())
            .merge(documents::router())
            .merge(analyses::router())
            .merge(reports::router()),
    )
}

pub async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(json!({ "status": "ok" })))
}

pub async fn readyz(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .map_err(CoreError::Database)?;
    Ok((StatusCode::OK, axum::Json(json!({ "status": "ready" }))))
}

pub async fn metrics(State(state): State<AppState>) -> Result<Response<Body>, ApiError> {
    state.metrics.refresh(&state.pool).await?;
    let bytes = state.metrics.encode()?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Body::from(bytes))
        .map_err(|error| ApiError(CoreError::Internal(anyhow::Error::new(error))))
}
