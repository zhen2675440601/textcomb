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
use std::str::FromStr;
use textcomb_core::{CoreError, ErrorCode, db, reporting};
use textcomb_domain::{
    FeedbackVerdict, Issue, IssueCategory, IssueLevel, MissedIssueFeedback, ReportV1,
};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/{id}/source", get(get_source))
        .route("/reports/{id}", get(get_report).delete(delete_report))
        .route("/reports/{id}/issues", get(list_issues))
        .route("/reports/{id}/misses", post(add_missed_issue))
        .route(
            "/reports/{id}/misses/{miss_id}",
            axum::routing::delete(delete_missed_issue),
        )
        .route("/reports/{id}/export/{format}", get(export))
        .route("/issues/{id}/feedback", post(feedback))
}

#[derive(Debug, Serialize)]
struct ReportSourceResponse {
    original_name: String,
    document_format: String,
    char_count: u32,
    char_start: u32,
    char_end: u32,
    text: String,
}

#[derive(Debug, Deserialize)]
struct SourceQuery {
    start: Option<u32>,
    end: Option<u32>,
}

async fn get_source(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
    Query(query): Query<SourceQuery>,
) -> ApiResult<Json<ReportSourceResponse>> {
    let source = db::load_report_source(&state.pool, report_id, user.id).await?;
    let text = source.extracted_text.ok_or_else(|| {
        ApiError(CoreError::public(
            ErrorCode::NotFound,
            "这份历史报告的分析原文已按旧策略清理；请重新分析后查看",
        ))
    })?;
    let chars: Vec<char> = text.chars().collect();
    let char_count = source
        .char_count
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_else(|| u32::try_from(chars.len()).unwrap_or(u32::MAX));
    let total = chars.len();
    let start = usize::try_from(query.start.unwrap_or(0))
        .unwrap_or(0)
        .min(total);
    let requested_end = query
        .end
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| start.saturating_add(8_000));
    let end = requested_end
        .max(start)
        .min(total)
        .min(start.saturating_add(12_000));
    let window: String = chars[start..end].iter().collect();
    Ok(Json(ReportSourceResponse {
        original_name: source.original_name,
        document_format: source.document_format,
        char_count,
        char_start: u32::try_from(start).unwrap_or(u32::MAX),
        char_end: u32::try_from(end).unwrap_or(u32::MAX),
        text: window,
    }))
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
    let mut report: ReportV1 = match serde_json::from_value(record.content_json) {
        Ok(report) => report,
        Err(error) => {
            tracing::error!(
                report_id = %report_id,
                error = %error,
                "stored report JSON does not match ReportV1"
            );
            return Err(ApiError(CoreError::public(
                ErrorCode::ModelOutputInvalid,
                "报告数据无法解析，请重新分析文章",
            )));
        }
    };
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
    let misses: Vec<(Uuid, String, i32, i32, String, String, time::OffsetDateTime)> =
        sqlx::query_as(
            r#"
            SELECT id, category, char_start, char_end, quote, note, created_at
            FROM missed_issue_feedback
            WHERE report_id = $1 AND user_id = $2
            ORDER BY created_at, id
            "#,
        )
        .bind(report_id)
        .bind(user_id)
        .fetch_all(&state.pool)
        .await?;
    report.missed_issues = misses
        .into_iter()
        .filter_map(|(id, category, start, end, quote, note, created_at)| {
            Some(MissedIssueFeedback {
                id,
                category: IssueCategory::from_str(&category).ok()?,
                char_start: u32::try_from(start).ok()?,
                char_end: u32::try_from(end).ok()?,
                quote,
                note,
                created_at,
            })
        })
        .collect();
    Ok(report)
}

#[derive(Debug, Deserialize)]
struct MissedIssueRequest {
    category: IssueCategory,
    char_start: u32,
    char_end: u32,
    note: String,
}

async fn add_missed_issue(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
    Json(input): Json<MissedIssueRequest>,
) -> ApiResult<(StatusCode, Json<MissedIssueFeedback>)> {
    let source = db::load_report_source(&state.pool, report_id, user.id).await?;
    let text = source.extracted_text.ok_or_else(|| {
        ApiError(CoreError::public(
            ErrorCode::NotFound,
            "分析原文已清理，无法标记漏检",
        ))
    })?;
    let chars: Vec<char> = text.chars().collect();
    let start = input.char_start as usize;
    let end = input.char_end as usize;
    if start >= end || end > chars.len() || end - start > 300 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "请选择原文中不超过 300 字的漏检片段",
        )));
    }
    let note = input.note.trim();
    if note.is_empty() || note.chars().count() > 1_000 {
        return Err(ApiError(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "请填写 1–1000 字的漏检说明",
        )));
    }
    let id = Uuid::new_v4();
    let quote: String = chars[start..end].iter().collect();
    let created_at: time::OffsetDateTime = sqlx::query_scalar(
        r#"
        INSERT INTO missed_issue_feedback(id, report_id, user_id, category, char_start, char_end, quote, note)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING created_at
        "#,
    )
    .bind(id)
    .bind(report_id)
    .bind(user.id)
    .bind(input.category.as_str())
    .bind(input.char_start as i32)
    .bind(input.char_end as i32)
    .bind(&quote)
    .bind(note)
    .fetch_one(&state.pool)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(MissedIssueFeedback {
            id,
            category: input.category,
            char_start: input.char_start,
            char_end: input.char_end,
            quote,
            note: note.to_owned(),
            created_at,
        }),
    ))
}

async fn delete_missed_issue(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((report_id, miss_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let result = sqlx::query(
        "DELETE FROM missed_issue_feedback WHERE id = $1 AND report_id = $2 AND user_id = $3",
    )
    .bind(miss_id)
    .bind(report_id)
    .bind(user.id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError(CoreError::public(
            ErrorCode::NotFound,
            "漏检反馈不存在",
        )));
    }
    Ok(StatusCode::NO_CONTENT)
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
    let report = load_enriched_report(&state, user.id, report_id).await?;
    match format.as_str() {
        "json" => {
            let bytes = serde_json::to_vec_pretty(&report)?;
            attachment(
                bytes,
                "application/json; charset=utf-8",
                "textcomb-report.json",
            )
        }
        "md" | "markdown" => attachment(
            reporting::to_markdown(&report).into_bytes(),
            "text/markdown; charset=utf-8",
            "textcomb-report.md",
        ),
        "pdf" => {
            let export_id = Uuid::new_v4();
            let path = state.storage.temporary_path(export_id, "pdf");
            let source_path = state.storage.temporary_path(export_id, "typ");
            reporting::render_pdf(&report, &path, &source_path).await?;
            let result = state.storage.read(&path).await;
            state.storage.remove(&path).await?;
            let bytes = result?;
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

pub(super) async fn delete_report(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(report_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    super::deletion::remove(&state, user.id, super::deletion::Target::Report(report_id)).await
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
