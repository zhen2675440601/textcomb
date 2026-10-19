use super::analyses::AnalysisResponse;
use crate::{
    problem::{ApiError, ApiResult},
    session::AuthUser,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use textcomb_core::{CoreError, ErrorCode};

pub fn router() -> Router<AppState> {
    Router::new().route("/analyses/history", get(page))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HistoryQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    status: Option<String>,
    q: Option<String>,
}

impl HistoryQuery {
    fn validate(&self) -> ApiResult<(i64, i64, String, String)> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(20);
        let status = self.status.as_deref().unwrap_or("all");
        let search = self.q.as_deref().unwrap_or("").trim();
        if page == 0
            || !(1..=100).contains(&size)
            || ![
                "all",
                "active",
                "completed",
                "failed",
                "cancelled",
                "expired",
            ]
            .contains(&status)
            || search.chars().count() > 100
        {
            return Err(ApiError(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "分页参数无效；每页最多 100 条，搜索内容最多 100 字",
            )));
        }
        // Treat user-entered % and _ as literal filename characters.
        let pattern = format!(
            "%{}%",
            search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        Ok((i64::from(page), i64::from(size), status.to_owned(), pattern))
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HistoryItem {
    #[serde(flatten)]
    #[sqlx(flatten)]
    analysis: AnalysisResponse,
    original_name: String,
    document_format: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HistoryTotals {
    total: i64,
    active: i64,
    completed: i64,
    failed: i64,
}

#[derive(Debug, Serialize)]
pub struct HistoryPage {
    items: Vec<HistoryItem>,
    total: i64,
    page: i64,
    page_size: i64,
    totals: HistoryTotals,
}

async fn page(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<HistoryPage>> {
    let (page, page_size, status, pattern) = query.validate()?;
    let mut tx = state.pool.begin().await?;
    // A worker may finish between these reads. Keep the count and page consistent.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let totals = sqlx::query_as::<_, HistoryTotals>(r#"
        SELECT count(*) AS total,
            count(*) FILTER (WHERE status NOT IN ('completed','failed','cancelled','expired')) AS active,
            count(*) FILTER (WHERE status = 'completed') AS completed,
            count(*) FILTER (WHERE status = 'failed') AS failed
        FROM analysis_jobs WHERE user_id = $1
    "#).bind(user.id).fetch_one(&mut *tx).await?;
    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM analysis_jobs j
        JOIN documents d ON d.id = j.document_id AND d.user_id = j.user_id
        WHERE j.user_id = $1 AND d.original_name ILIKE $2 ESCAPE '\'
          AND ($3 = 'all' OR j.status = $3 OR
            ($3 = 'active' AND j.status NOT IN ('completed','failed','cancelled','expired')))
    "#,
    )
    .bind(user.id)
    .bind(&pattern)
    .bind(&status)
    .fetch_one(&mut *tx)
    .await?;
    let page = page.min(((total + page_size - 1) / page_size).max(1));
    let items = sqlx::query_as::<_, HistoryItem>(
        r#"
        SELECT j.id, j.document_id, j.model_profile_id, j.analysis_profile, j.status,
            j.stage, j.progress, j.total_chunks, j.completed_chunks, j.error_code,
            j.error_message, j.report_id, j.created_at, j.started_at, j.completed_at,
            d.original_name, d.document_format
        FROM analysis_jobs j
        JOIN documents d ON d.id = j.document_id AND d.user_id = j.user_id
        WHERE j.user_id = $1 AND d.original_name ILIKE $2 ESCAPE '\'
          AND ($3 = 'all' OR j.status = $3 OR
            ($3 = 'active' AND j.status NOT IN ('completed','failed','cancelled','expired')))
        ORDER BY j.created_at DESC, j.id DESC LIMIT $4 OFFSET $5
    "#,
    )
    .bind(user.id)
    .bind(&pattern)
    .bind(&status)
    .bind(page_size)
    .bind((page - 1) * page_size)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(HistoryPage {
        items,
        total,
        page,
        page_size,
        totals,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiMetrics;
    use sqlx::{PgPool, postgres::PgPoolOptions};
    use std::sync::Arc;
    use textcomb_core::{AppConfig, auth::UserIdentity, db, storage::LocalStorage};
    use uuid::Uuid;

    #[test]
    fn history_query_bounds_and_literal_search() {
        assert!(
            HistoryQuery {
                page: Some(0),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HistoryQuery {
                page_size: Some(101),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HistoryQuery {
                status: Some("unknown".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            HistoryQuery {
                q: Some("  100%_\\  ".into()),
                ..Default::default()
            }
            .validate()
            .unwrap()
            .3,
            "%100\\%\\_\\\\%"
        );
    }

    #[tokio::test]
    async fn history_pagination_search_totals_and_owner_isolation() {
        let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("history_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .after_connect(move |connection, _| {
                let statement = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        db::migrate(&pool).await.unwrap();
        let root = std::env::temp_dir().join(format!("textcomb-history-{}", Uuid::new_v4()));
        let state = AppState {
            pool: pool.clone(),
            config: Arc::new(AppConfig::from_env().unwrap()),
            storage: LocalStorage::new(&root).await.unwrap(),
            metrics: ApiMetrics::new().unwrap(),
        };
        let user = AuthUser(UserIdentity {
            id: Uuid::new_v4(),
            username: "history-owner".into(),
            role: "user".into(),
        });
        for (id, name, count) in [
            (user.0.id, "chapter 100%_a.txt", 205),
            (Uuid::new_v4(), "other.txt", 7),
        ] {
            let profile = Uuid::new_v4();
            let document = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO users(id,username,password_hash,role) VALUES($1,$2,'unused','user')",
            )
            .bind(id)
            .bind(id.to_string())
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO model_profiles(id,owner_id,name,base_url,api_key_ciphertext,candidate_model,verifier_model,disclosure_accepted_at) VALUES($1,$2,'test','https://example.test',decode('00','hex'),'test','test',now())")
                .bind(profile).bind(id).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO documents(id,user_id,original_name,media_type,document_format,size_bytes) VALUES($1,$2,$3,'text/plain','txt',12)")
                .bind(document).bind(id).bind(name).execute(&pool).await.unwrap();
            for index in 0..count {
                sqlx::query("INSERT INTO analysis_jobs(id,user_id,document_id,model_profile_id,status,analyzer_version,timeout_at,created_at) VALUES($1,$2,$3,$4,$5,'test',now() + interval '1 hour','2026-01-01T00:00:00Z')")
                    .bind(Uuid::new_v4()).bind(id).bind(document).bind(profile)
                    .bind(if index == 0 { "cancel_requested" } else { "completed" })
                    .execute(&pool).await.unwrap();
            }
        }
        let Json(first) = page(
            State(state.clone()),
            user.clone(),
            Query(HistoryQuery::default()),
        )
        .await
        .unwrap();
        assert_eq!(first.totals.total, 205);
        assert_eq!(first.totals.active, 1);
        assert_eq!(first.totals.completed, 204);
        assert_eq!(first.total, 205);
        assert_eq!(first.items.len(), 20);
        assert!(
            first
                .items
                .iter()
                .all(|item| item.original_name == "chapter 100%_a.txt")
        );
        let Json(second) = page(
            State(state.clone()),
            user.clone(),
            Query(HistoryQuery {
                page: Some(2),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert!(
            first
                .items
                .iter()
                .all(|a| second.items.iter().all(|b| a.analysis.id != b.analysis.id))
        );
        let Json(last) = page(
            State(state.clone()),
            user.clone(),
            Query(HistoryQuery {
                page: Some(99),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(last.page, 11);
        assert_eq!(last.items.len(), 5);
        for (q, expected) in [("100%_a", 205), ("100%_b", 0), ("other", 0)] {
            let Json(found) = page(
                State(state.clone()),
                user.clone(),
                Query(HistoryQuery {
                    q: Some(q.into()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
            assert_eq!(found.total, expected);
        }
        let Json(active) = page(
            State(state.clone()),
            user,
            Query(HistoryQuery {
                status: Some("active".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(active.total, 1);
        assert_eq!(active.items[0].analysis.status, "cancel_requested");
        assert_eq!(active.totals.total, 205);
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
        admin.close().await;
    }
}
