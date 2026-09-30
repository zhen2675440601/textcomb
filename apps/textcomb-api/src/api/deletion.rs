use crate::{
    problem::{ApiError, ApiResult},
    state::AppState,
};
use axum::http::StatusCode;
use textcomb_core::{CoreError, ErrorCode};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(super) enum Target {
    Document(Uuid),
    Analysis(Uuid),
    Report(Uuid),
}

#[derive(sqlx::FromRow)]
struct LockedJob {
    id: Uuid,
    status: String,
}

#[derive(sqlx::FromRow)]
struct LockedReport {
    id: Uuid,
    job_id: Uuid,
    pdf_path: Option<String>,
}

fn conflict(message: &'static str) -> ApiError {
    ApiError(CoreError::public(ErrorCode::Conflict, message))
}

pub(super) async fn remove(
    state: &AppState,
    user_id: Uuid,
    target: Target,
) -> ApiResult<StatusCode> {
    let mut transaction = state.pool.begin().await?;
    let document_id = match target {
        Target::Document(id) => id,
        Target::Analysis(id) => sqlx::query_scalar::<_, Uuid>(
            "SELECT document_id FROM analysis_jobs WHERE id = $1 AND user_id = $2",
        ).bind(id).bind(user_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ApiError(CoreError::public(ErrorCode::NotFound, "分析任务不存在")))?,
        Target::Report(id) => sqlx::query_scalar::<_, Uuid>(
            "SELECT job.document_id FROM reports AS report JOIN analysis_jobs AS job ON job.id = report.job_id WHERE report.id = $1 AND report.user_id = $2 AND job.user_id = $2",
        ).bind(id).bind(user_id).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| ApiError(CoreError::public(ErrorCode::NotFound, "报告不存在")))?,
    };
    // Report expiry uses report -> document ordering. Lock every related report
    // before jobs/documents; UUID ordering also serializes overlapping deletions.
    let reports = sqlx::query_as::<_, LockedReport>(
        "SELECT report.id, report.job_id, report.pdf_path FROM reports AS report JOIN analysis_jobs AS job ON job.id = report.job_id WHERE job.document_id = $1 AND job.user_id = $2 AND report.user_id = $2 ORDER BY report.id FOR UPDATE OF report",
    ).bind(document_id).bind(user_id).fetch_all(&mut *transaction).await?;
    let jobs = sqlx::query_as::<_, LockedJob>(
        "SELECT id, status FROM analysis_jobs WHERE document_id = $1 AND user_id = $2 ORDER BY id FOR UPDATE",
    ).bind(document_id).bind(user_id).fetch_all(&mut *transaction).await?;
    let input_path: Option<Option<String>> = sqlx::query_scalar(
        "SELECT storage_path FROM documents WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(document_id)
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let input_path =
        input_path.ok_or_else(|| ApiError(CoreError::public(ErrorCode::NotFound, "文档不存在")))?;

    // Creation locks the document first. A newly committed job/report may have
    // appeared before our document lock; reject rather than invert lock order.
    let current_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM analysis_jobs WHERE document_id = $1 AND user_id = $2 ORDER BY id",
    )
    .bind(document_id)
    .bind(user_id)
    .fetch_all(&mut *transaction)
    .await?;
    if current_ids != jobs.iter().map(|job| job.id).collect::<Vec<_>>() {
        return Err(conflict("文档关联任务发生变化，请刷新后重试删除"));
    }
    let report_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT report.id FROM reports AS report JOIN analysis_jobs AS job ON job.id = report.job_id WHERE job.document_id = $1 AND job.user_id = $2 AND report.user_id = $2 ORDER BY report.id",
    ).bind(document_id).bind(user_id).fetch_all(&mut *transaction).await?;
    if report_ids != reports.iter().map(|report| report.id).collect::<Vec<_>>() {
        return Err(conflict("报告状态发生变化，请刷新后重试删除"));
    }
    if jobs.iter().any(|job| {
        !matches!(
            job.status.as_str(),
            "completed" | "failed" | "cancelled" | "expired"
        )
    }) {
        return Err(conflict("请先取消相关任务并等待取消完成，再执行删除"));
    }
    match target {
        Target::Analysis(id) if !jobs.iter().any(|job| job.id == id) => {
            return Err(ApiError(CoreError::public(
                ErrorCode::NotFound,
                "分析任务不存在",
            )));
        }
        Target::Report(id) if !reports.iter().any(|report| report.id == id) => {
            return Err(ApiError(CoreError::public(
                ErrorCode::NotFound,
                "报告不存在",
            )));
        }
        _ => {}
    }
    let selected_job_id = match target {
        Target::Document(_) => None,
        Target::Analysis(id) => Some(id),
        Target::Report(id) => reports
            .iter()
            .find(|report| report.id == id)
            .map(|report| report.job_id),
    };
    // Terminal metadata alone does not need the source. Preserve it only for
    // another report or a failed job that may still be retried; otherwise the
    // last deleted report in a legacy shared document would leave body data.
    let other_source_consumer = jobs
        .iter()
        .any(|job| Some(job.id) != selected_job_id && job.status == "failed")
        || reports
            .iter()
            .any(|report| Some(report.job_id) != selected_job_id);
    let delete_source = matches!(target, Target::Document(_)) || !other_source_consumer;
    if delete_source && let Some(path) = &input_path {
        state.storage.remove(std::path::Path::new(path)).await?;
    }
    for report in &reports {
        let selected = match target {
            Target::Document(_) => true,
            Target::Analysis(id) => report.job_id == id,
            Target::Report(id) => report.id == id,
        };
        if selected && let Some(path) = &report.pdf_path {
            state.storage.remove(std::path::Path::new(path)).await?;
        }
    }
    match target {
        Target::Document(_) => {
            sqlx::query("DELETE FROM documents WHERE id = $1 AND user_id = $2")
                .bind(document_id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?;
        }
        Target::Analysis(id) => {
            sqlx::query("DELETE FROM analysis_jobs WHERE id = $1 AND user_id = $2")
                .bind(id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?;
            if jobs.len() <= 1 {
                sqlx::query("DELETE FROM documents WHERE id = $1 AND user_id = $2")
                    .bind(document_id)
                    .bind(user_id)
                    .execute(&mut *transaction)
                    .await?;
            } else if delete_source {
                sqlx::query("UPDATE documents SET extracted_text = NULL, storage_path = NULL, input_expires_at = NULL WHERE id = $1 AND user_id = $2")
                    .bind(document_id).bind(user_id).execute(&mut *transaction).await?;
            }
        }
        Target::Report(id) => {
            sqlx::query("DELETE FROM reports WHERE id = $1 AND user_id = $2")
                .bind(id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("UPDATE analysis_jobs SET status = 'expired', stage = 'expired', report_id = NULL WHERE document_id = $1 AND user_id = $2 AND id = $3")
                .bind(document_id).bind(user_id).bind(selected_job_id)
                .execute(&mut *transaction).await?;
            if delete_source {
                sqlx::query("UPDATE documents SET extracted_text = NULL, storage_path = NULL, input_expires_at = NULL WHERE id = $1 AND user_id = $2")
                    .bind(document_id).bind(user_id).execute(&mut *transaction).await?;
            }
        }
    }
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
