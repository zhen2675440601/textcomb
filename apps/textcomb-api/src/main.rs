mod api;
mod openapi;
mod problem;
mod session;
mod state;

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderName, HeaderValue, Method, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use std::{net::SocketAddr, sync::Arc};
use textcomb_core::{AppConfig, db, storage::LocalStorage};
use tower_http::{catch_panic::CatchPanicLayer, cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

use crate::state::{ApiMetrics, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    let config = Arc::new(AppConfig::from_env()?);
    let pool = db::connect(&config.database_url).await?;
    let storage = LocalStorage::new(config.storage_dir()).await?;
    let metrics = ApiMetrics::new()?;
    let state = AppState {
        pool,
        config: config.clone(),
        storage,
        metrics,
    };

    let request_id_header = HeaderName::from_static("x-request-id");
    let app = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/readyz", get(api::readyz))
        .route("/metrics", get(api::metrics))
        .merge(api::router())
        .merge(openapi::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_context,
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(origin_header(&config.public_origin)?)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                ])
                .allow_headers([
                    http::header::CONTENT_TYPE,
                    http::header::ACCEPT,
                    HeaderName::from_static("idempotency-key"),
                    request_id_header.clone(),
                ])
                .allow_credentials(true),
        )
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    info!(address = %config.bind, "TextComb API listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("TEXTCOMB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,textcomb=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

fn origin_header(origin: &Url) -> anyhow::Result<http::HeaderValue> {
    Ok(http::HeaderValue::from_str(
        &origin.origin().ascii_serialization(),
    )?)
}

async fn request_context(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 100 && value.is_ascii())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    problem::REQUEST_ID
        .scope(request_id.clone(), async move {
            if is_cross_site_mutation(&request, &state.config.public_origin) {
                return problem::ApiError(textcomb_core::CoreError::public(
                    textcomb_core::ErrorCode::Forbidden,
                    "跨站请求已被拒绝",
                ))
                .into_response();
            }
            let mut response = next.run(request).await;
            if let Ok(value) = HeaderValue::from_str(&request_id) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("x-request-id"), value);
            }
            response
        })
        .await
}

fn is_cross_site_mutation(request: &Request, expected_origin: &Url) -> bool {
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return false;
    }
    if request
        .headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return true;
    }
    let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin != expected_origin.origin().ascii_serialization()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
