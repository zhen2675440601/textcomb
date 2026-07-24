use crate::{
    problem::{ApiError, ApiResult},
    session::AuthUser,
    state::AppState,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode, header},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{path::Path as FilePath, str::FromStr};
use textcomb_core::{CoreError, ErrorCode, db};
use textcomb_domain::{FeedbackVerdict, Issue, IssueCategory, IssueLevel, ReportV1};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/{id}", get(get_report).delete(delete_report))
        .route("/reports/{id}/issues", get(list_issues))
        .route("/reports/{id}/export/{format}", get(export))
        .route("/issues/{id}/feedback", post(feedback))
}

async fn get_report(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
) -> ApiResult<Json<ReportV1>> {
    Ok(Json(
        load_enriched_report(&state, user.id, report_id).await?,
    ))
}

async fn load_enriched_report(
    state: &AppState,
    user_id: Uuid,
    report_id: Uuid,
) -> ApiResult<ReportV1> {
    let record = db::load_report(&state.pool, report_id, user_id).await?;
    let mut report: ReportV1 = serde_json::from_value(record.content_json)?;
    let feedback: Vec<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT issue_feedback.issue_id, issue_feedback.verdict
        FROM issue_feedback
        JOIN issues ON issues.id = issue_feedback.issue_id
        WHERE issues.report_id = $1 AND issue_feedback.user_id = $2
        "#,
    )
    .bind(report_id)
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    for (issue_id, verdict) in feedback {
        if let Some(issue) = report.issues.iter_mut().find(|issue| issue.id == issue_id) {
            issue.feedback = FeedbackVerdict::from_str(&verdict).ok();
        }
    }
    Ok(report)
}

#[derive(Debug, Deserialize)]
struct IssueListQuery {
    page: Option<usize>,
    page_size: Option<usize>,
    category: Option<String>,
    level: Option<String>,
    min_confidence: Option<u8>,
    feedback: Option<String>,
}

#[derive(Debug, Serialize)]
struct IssuePage {
    items: Vec<Issue>,
    total: usize,
    page: usize,
    page_size: usize,
}

async fn list_issues(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
    Query(query): Query<IssueListQuery>,
) -> ApiResult<Json<IssuePage>> {
    let category = query
        .category
        .as_deref()
        .map(IssueCategory::from_str)
        .transpose()
        .map_err(|_| {
            ApiError(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "问题类型筛选值无效",
            ))
        })?;
    let level = query
        .level
        .as_deref()
        .map(IssueLevel::from_str)
        .transpose()
        .map_err(|_| {
            ApiError(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "问题级别筛选值无效",
            ))
        })?;
    let feedback = match query.feedback.as_deref() {
        None | Some("all") => None,
        Some("unreviewed") => Some(None),
        Some(value) => Some(Some(FeedbackVerdict::from_str(value).map_err(|_| {
            ApiError(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "反馈筛选值无效",
            ))
        })?)),
    };
    let report = load_enriched_report(&state, user.id, report_id).await?;
    let minimum = query.min_confidence.unwrap_or(0).min(100);
    let filtered: Vec<Issue> = report
        .issues
        .into_iter()
        .filter(|issue| category.is_none_or(|value| issue.category == value))
        .filter(|issue| level.is_none_or(|value| issue.level == value))
        .filter(|issue| issue.confidence >= minimum)
        .filter(|issue| match feedback {
            None => true,
            Some(None) => issue.feedback.is_none(),
            Some(Some(value)) => issue.feedback == Some(value),
        })
        .collect();
    let total = filtered.len();
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(50).clamp(1, 100);
    let start = (page - 1).saturating_mul(page_size).min(total);
    let items = filtered.into_iter().skip(start).take(page_size).collect();
    Ok(Json(IssuePage {
        items,
        total,
        page,
        page_size,
    }))
}

async fn export(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((report_id, format)): Path<(Uuid, String)>,
) -> ApiResult<Response<Body>> {
    let record = db::load_report(&state.pool, report_id, user.id).await?;
    match format.as_str() {
        "json" => {
            let bytes = serde_json::to_vec_pretty(&record.content_json)?;
            attachment(
                bytes,
                "application/json; charset=utf-8",
                "textcomb-report.json",
            )
        }
        "md" | "markdown" => attachment(
            record.markdown.into_bytes(),
            "text/markdown; charset=utf-8",
            "textcomb-report.md",
        ),
        "pdf" => {
            let path = record.pdf_path.ok_or_else(|| {
                ApiError(CoreError::public(ErrorCode::NotFound, "PDF 报告不存在"))
            })?;
            let bytes = tokio::fs::read(path).await?;
            attachment(bytes, "application/pdf", "textcomb-report.pdf")
        }
        _ => Err(ApiError(CoreError::public(
            ErrorCode::UnsupportedFormat,
            "仅支持 json、md 和 pdf 导出",
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct FeedbackRequest {
    verdict: String,
    note: Option<String>,
}

async fn feedback(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(issue_id): Path<Uuid>,
    Json(input): Json<FeedbackRequest>,
) -> ApiResult<StatusCode> {
    let verdict = FeedbackVerdict::from_str(&input.verdict).map_err(|_| {
        ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "反馈只能是 correct、incorrect 或 disputed",
        ))
    })?;
    if input
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 1000)
    {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "反馈备注不能超过 1000 个字符",
        )));
    }
    let feedback_id = Uuid::new_v4();
    let result = sqlx::query(
        r#"
        INSERT INTO issue_feedback(id, issue_id, user_id, verdict, note)
        SELECT $1, issues.id, $2, $3, $4
        FROM issues
        JOIN reports ON reports.id = issues.report_id
        WHERE issues.id = $5 AND reports.user_id = $2
        ON CONFLICT(issue_id, user_id) DO UPDATE SET
            verdict = EXCLUDED.verdict,
            note = EXCLUDED.note,
            updated_at = now()
        "#,
    )
    .bind(feedback_id)
    .bind(user.id)
    .bind(verdict.as_str())
    .bind(input.note.as_deref())
    .bind(issue_id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "问题不存在",
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_report(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut transaction = state.pool.begin().await?;
    let report: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT job_id, pdf_path FROM reports WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(report_id)
    .bind(user.id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((job_id, path)) = report else {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "报告不存在",
        )));
    };
    if let Some(path) = path.as_deref() {
        state.storage.remove(FilePath::new(path)).await?;
    }
    sqlx::query("DELETE FROM reports WHERE id = $1 AND user_id = $2")
        .bind(report_id)
        .bind(user.id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "UPDATE analysis_jobs SET status = 'expired', stage = 'expired', report_id = NULL WHERE id = $1 AND user_id = $2",
    )
    .bind(job_id)
    .bind(user.id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn attachment(
    bytes: Vec<u8>,
    content_type: &'static str,
    filename: &'static str,
) -> ApiResult<Response<Body>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .map_err(|error| ApiError(CoreError::Internal(anyhow::Error::new(error))))
}
