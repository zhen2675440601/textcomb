use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use textcomb_core::{CoreError, ErrorCode};
use uuid::Uuid;

tokio::task_local! {
    pub static REQUEST_ID: String;
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError(pub CoreError);

impl From<CoreError> for ApiError {
    fn from(error: CoreError) -> Self {
        Self(error)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self(CoreError::Database(error))
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        Self(CoreError::Io(error))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self(CoreError::Json(error))
    }
}

#[derive(Serialize)]
struct ProblemDetails {
    #[serde(rename = "type")]
    problem_type: String,
    title: &'static str,
    status: u16,
    code: &'static str,
    detail: String,
    request_id: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.0.code();
        let status = status_for(code);
        let request_id = REQUEST_ID
            .try_with(Clone::clone)
            .unwrap_or_else(|_| Uuid::now_v7().to_string());
        tracing::warn!(request_id, error_code = code.as_str(), "API request failed");
        let body = ProblemDetails {
            problem_type: format!(
                "https://textcomb.dev/problems/{}",
                code.as_str().to_lowercase()
            ),
            title: "请求无法完成",
            status: status.as_u16(),
            code: code.as_str(),
            detail: self.0.safe_message().into_owned(),
            request_id,
        };
        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::FileTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::TextTooLong
        | ErrorCode::UnsupportedFormat
        | ErrorCode::PdfScanned
        | ErrorCode::PdfEncrypted
        | ErrorCode::ExtractionFailed
        | ErrorCode::ModelOutputInvalid
        | ErrorCode::ConfigurationInvalid => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorCode::ProviderUnavailable => StatusCode::BAD_GATEWAY,
        ErrorCode::JobTimeout => StatusCode::GATEWAY_TIMEOUT,
        ErrorCode::JobCancelled => StatusCode::CONFLICT,
        ErrorCode::StorageFailed
        | ErrorCode::DatabaseFailed
        | ErrorCode::ReportRenderFailed
        | ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
