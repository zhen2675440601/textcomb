use std::{borrow::Cow, io};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ConfigurationInvalid,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    FileTooLarge,
    TextTooLong,
    UnsupportedFormat,
    PdfScanned,
    PdfEncrypted,
    ExtractionFailed,
    ProviderUnavailable,
    ModelOutputInvalid,
    JobTimeout,
    JobCancelled,
    StorageFailed,
    DatabaseFailed,
    ReportRenderFailed,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "CONFIGURATION_INVALID",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict => "CONFLICT",
            Self::RateLimited => "RATE_LIMITED",
            Self::FileTooLarge => "FILE_TOO_LARGE",
            Self::TextTooLong => "TEXT_TOO_LONG",
            Self::UnsupportedFormat => "UNSUPPORTED_FORMAT",
            Self::PdfScanned => "PDF_SCANNED_OR_NO_TEXT",
            Self::PdfEncrypted => "PDF_ENCRYPTED",
            Self::ExtractionFailed => "EXTRACTION_FAILED",
            Self::ProviderUnavailable => "PROVIDER_UNAVAILABLE",
            Self::ModelOutputInvalid => "MODEL_OUTPUT_INVALID",
            Self::JobTimeout => "JOB_TIMEOUT",
            Self::JobCancelled => "JOB_CANCELLED",
            Self::StorageFailed => "STORAGE_FAILED",
            Self::DatabaseFailed => "DATABASE_FAILED",
            Self::ReportRenderFailed => "REPORT_RENDER_FAILED",
            Self::Internal => "INTERNAL_ERROR",
        }
    }
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{message}")]
    Public {
        code: ErrorCode,
        message: Cow<'static, str>,
    },
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("I/O operation failed")]
    Io(#[from] io::Error),
    #[error("JSON operation failed")]
    Json(#[from] serde_json::Error),
    #[error("internal operation failed: {0}")]
    Internal(#[from] anyhow::Error),
}

impl CoreError {
    pub fn public(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Public {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Public { code, .. } => *code,
            Self::Database(_) => ErrorCode::DatabaseFailed,
            Self::Io(_) => ErrorCode::StorageFailed,
            Self::Json(_) => ErrorCode::ModelOutputInvalid,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn safe_message(&self) -> Cow<'static, str> {
        match self {
            Self::Public { message, .. } => message.clone(),
            Self::Database(_) => Cow::Borrowed("数据库操作失败"),
            Self::Io(_) => Cow::Borrowed("文件或存储操作失败"),
            Self::Json(_) => Cow::Borrowed("模型返回了无法解析的结构"),
            Self::Internal(_) => Cow::Borrowed("内部处理失败"),
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;
