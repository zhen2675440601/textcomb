use crate::{
    problem::{ApiError, ApiResult},
    session::AuthUser,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use std::path::Path as FilePath;
use textcomb_core::{
    CoreError, ErrorCode,
    documents::{self, MAX_TEXT_CHARS, MAX_UPLOAD_BYTES},
};
use time::OffsetDateTime;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/documents",
            get(list)
                .post(upload)
                .layer(DefaultBodyLimit::max(21 * 1024 * 1024)),
        )
        .route(
            "/documents/text",
            post(create_text).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/documents/{id}", delete(remove))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DocumentResponse {
    pub id: Uuid,
    pub original_name: String,
    pub media_type: String,
    pub document_format: String,
    pub size_bytes: i64,
    pub char_count: Option<i32>,
    pub content_available: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

struct PendingDocument<'a> {
    filename: &'a str,
    media_type: &'a str,
    format: &'a str,
    bytes: &'a [u8],
    char_count: Option<i32>,
    operation: &'static str,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<DocumentResponse>>> {
    let rows = sqlx::query_as::<_, DocumentResponse>(
        r#"
        SELECT id, original_name, media_type, document_format, size_bytes, char_count,
               storage_path IS NOT NULL AND input_expires_at > now() AS content_available, created_at
        FROM documents WHERE user_id = $1 ORDER BY created_at DESC LIMIT 200
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

async fn upload(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<DocumentResponse>)> {
    let mut uploaded: Option<(String, String, bytes::Bytes)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError(CoreError::Internal(anyhow::Error::new(error))))?
    {
        if field.name() != Some("file") {
            continue;
        }
        if uploaded.is_some() {
            return Err(ApiError(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "每次只能上传一个文件",
            )));
        }
        let filename = field
            .file_name()
            .map(safe_filename)
            .unwrap_or_else(|| "document.txt".to_owned());
        let media_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = field
            .bytes()
            .await
            .map_err(|error| ApiError(CoreError::Internal(anyhow::Error::new(error))))?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(ApiError(CoreError::public(
                ErrorCode::FileTooLarge,
                "文件超过 20 MiB 上限",
            )));
        }
        uploaded = Some((filename, media_type, bytes));
    }
    let (filename, media_type, bytes) = uploaded.ok_or_else(|| {
        ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "请求中缺少 file 字段",
        ))
    })?;
    let format = documents::detect_format(&filename, &bytes)?;
    documents::validate_media_type(format, &media_type)?;
    persist_document(
        &state,
        user.id,
        PendingDocument {
            filename: &filename,
            media_type: &media_type,
            format: format.as_str(),
            bytes: &bytes,
            char_count: None,
            operation: "document_upload",
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
struct CreateTextDocumentRequest {
    text: String,
}

async fn create_text(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateTextDocumentRequest>,
) -> ApiResult<(StatusCode, Json<DocumentResponse>)> {
    let char_count = validate_pasted_text(&input.text)?;
    let bytes = input.text.into_bytes();
    persist_document(
        &state,
        user.id,
        PendingDocument {
            filename: "粘贴文章.txt",
            media_type: "text/plain; charset=utf-8",
            format: "txt",
            bytes: &bytes,
            char_count: Some(char_count as i32),
            operation: "document_paste",
        },
    )
    .await
}

async fn persist_document(
    state: &AppState,
    user_id: Uuid,
    document: PendingDocument<'_>,
) -> ApiResult<(StatusCode, Json<DocumentResponse>)> {
    let document_id = Uuid::new_v4();
    let storage_path = state
        .storage
        .write_upload(document_id, document.format, document.bytes)
        .await?;
    let expires_at = OffsetDateTime::now_utc()
        + time::Duration::hours(state.config.failed_input_retention_hours);
    let row = sqlx::query_as::<_, DocumentResponse>(
        r#"
        INSERT INTO documents(
            id, user_id, original_name, media_type, document_format,
            size_bytes, char_count, storage_path, input_expires_at
        )
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
        RETURNING id, original_name, media_type, document_format, size_bytes,
                  char_count, storage_path IS NOT NULL AND input_expires_at > now() AS content_available, created_at
        "#,
    )
    .bind(document_id)
    .bind(user_id)
    .bind(document.filename)
    .bind(document.media_type)
    .bind(document.format)
    .bind(document.bytes.len() as i64)
    .bind(document.char_count)
    .bind(storage_path.to_string_lossy().as_ref())
    .bind(expires_at)
    .fetch_one(&state.pool)
    .await;
    match row {
        Ok(row) => {
            state
                .metrics
                .operations
                .with_label_values(&[document.operation, "success"])
                .inc();
            Ok((StatusCode::CREATED, Json(row)))
        }
        Err(error) => {
            let _ = state.storage.remove(&storage_path).await;
            Err(error.into())
        }
    }
}

fn validate_pasted_text(text: &str) -> Result<usize, ApiError> {
    if text.trim().is_empty() {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "请粘贴需要分析的文章正文",
        )));
    }
    let char_count = text.chars().count();
    if char_count > MAX_TEXT_CHARS {
        return Err(ApiError(CoreError::public(
            ErrorCode::TextTooLong,
            format!("正文超过 {MAX_TEXT_CHARS} 字上限"),
        )));
    }
    Ok(char_count)
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(document_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let storage_path: Option<String> =
        sqlx::query_scalar("SELECT storage_path FROM documents WHERE id = $1 AND user_id = $2")
            .bind(document_id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM documents WHERE id = $1 AND user_id = $2)")
            .bind(document_id)
            .bind(user.id)
            .fetch_one(&state.pool)
            .await?;
    if !exists {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "文档不存在",
        )));
    }
    let pdf_paths: Vec<Option<String>> = sqlx::query_scalar(
        r#"
        SELECT reports.pdf_path
        FROM reports
        JOIN analysis_jobs ON analysis_jobs.id = reports.job_id
        WHERE analysis_jobs.document_id = $1 AND reports.user_id = $2
        "#,
    )
    .bind(document_id)
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    if let Some(path) = storage_path.as_deref() {
        state.storage.remove(FilePath::new(path)).await?;
    }
    for path in pdf_paths.iter().flatten() {
        state.storage.remove(FilePath::new(path)).await?;
    }
    sqlx::query("DELETE FROM documents WHERE id = $1 AND user_id = $2")
        .bind(document_id)
        .bind(user.id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn safe_filename(value: &str) -> String {
    let filename = FilePath::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document.txt");
    filename
        .chars()
        .filter(|character| !character.is_control())
        .take(180)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_validation_counts_unicode_characters() {
        assert_eq!(validate_pasted_text("中文 A\n第二行").unwrap(), 8);
    }

    #[test]
    fn pasted_text_validation_rejects_blank_and_oversized_input() {
        assert!(validate_pasted_text(" \n\t ").is_err());
        assert!(validate_pasted_text(&"文".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }
}
