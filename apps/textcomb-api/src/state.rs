use prometheus::{Encoder, IntCounterVec, IntGauge, IntGaugeVec, Registry, TextEncoder, opts};
use sqlx::PgPool;
use std::sync::Arc;
use textcomb_core::{AppConfig, error::CoreResult, storage::LocalStorage};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<AppConfig>,
    pub storage: LocalStorage,
    pub metrics: ApiMetrics,
}

#[derive(Clone)]
pub struct ApiMetrics {
    registry: Registry,
    pub operations: IntCounterVec,
    jobs: IntGaugeVec,
    oldest_queue_seconds: IntGauge,
    oldest_active_stage_seconds: IntGaugeVec,
    model_calls: IntGaugeVec,
    model_duration_ms: IntGaugeVec,
    model_retries: IntGauge,
    cleanup_pending: IntGaugeVec,
}

impl ApiMetrics {
    pub fn new() -> CoreResult<Self> {
        let registry = Registry::new();
        let operations = IntCounterVec::new(
            opts!("textcomb_api_operations_total", "API operation outcomes"),
            &["operation", "outcome"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        registry
            .register(Box::new(operations.clone()))
            .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let jobs = IntGaugeVec::new(
            opts!("textcomb_jobs", "Current analysis jobs by status"),
            &["status"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let oldest_queue_seconds = IntGauge::new(
            "textcomb_queue_oldest_wait_seconds",
            "Age in seconds of the oldest queued job",
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let oldest_active_stage_seconds = IntGaugeVec::new(
            opts!(
                "textcomb_active_stage_oldest_age_seconds",
                "Oldest active job age in seconds by stage"
            ),
            &["stage"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let model_calls = IntGaugeVec::new(
            opts!(
                "textcomb_model_calls",
                "Persisted model calls by pass and outcome"
            ),
            &["pass", "success"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let model_duration_ms = IntGaugeVec::new(
            opts!(
                "textcomb_model_duration_ms_total",
                "Persisted model call duration in milliseconds by pass"
            ),
            &["pass"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let model_retries = IntGauge::new(
            "textcomb_model_retry_attempts",
            "Persisted model retry attempts beyond the first call",
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        let cleanup_pending = IntGaugeVec::new(
            opts!(
                "textcomb_cleanup_pending",
                "Expired records awaiting file cleanup"
            ),
            &["kind"],
        )
        .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        for collector in [
            Box::new(jobs.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(oldest_queue_seconds.clone()),
            Box::new(oldest_active_stage_seconds.clone()),
            Box::new(model_calls.clone()),
            Box::new(model_duration_ms.clone()),
            Box::new(model_retries.clone()),
            Box::new(cleanup_pending.clone()),
        ] {
            registry
                .register(collector)
                .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        }
        Ok(Self {
            registry,
            operations,
            jobs,
            oldest_queue_seconds,
            oldest_active_stage_seconds,
            model_calls,
            model_duration_ms,
            model_retries,
            cleanup_pending,
        })
    }

    pub async fn refresh(&self, pool: &PgPool) -> CoreResult<()> {
        self.jobs.reset();
        let jobs: Vec<(String, i64)> =
            sqlx::query_as("SELECT status, count(*) FROM analysis_jobs GROUP BY status")
                .fetch_all(pool)
                .await?;
        for (status, count) in jobs {
            self.jobs.with_label_values(&[&status]).set(count);
        }

        let oldest_queue_seconds: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::bigint
            FROM analysis_jobs WHERE status = 'queued'
            "#,
        )
        .fetch_one(pool)
        .await?;
        self.oldest_queue_seconds.set(oldest_queue_seconds.max(0));

        self.oldest_active_stage_seconds.reset();
        let stage_ages: Vec<(String, i64)> = sqlx::query_as(
            r#"
            SELECT stage,
                   COALESCE(EXTRACT(EPOCH FROM now() - min(COALESCE(started_at, created_at))), 0)::bigint
            FROM analysis_jobs
            WHERE status IN ('extracting','analyzing','verifying','merging','rendering','cancel_requested')
            GROUP BY stage
            "#,
        )
        .fetch_all(pool)
        .await?;
        for (stage, age) in stage_ages {
            self.oldest_active_stage_seconds
                .with_label_values(&[&stage])
                .set(age.max(0));
        }

        self.model_calls.reset();
        self.model_duration_ms.reset();
        let model: Vec<(String, bool, i64, i64)> = sqlx::query_as(
            r#"
            SELECT pass, success, count(*), COALESCE(sum(duration_ms), 0)::bigint
            FROM model_usage GROUP BY pass, success
            "#,
        )
        .fetch_all(pool)
        .await?;
        for (pass, success, count, duration_ms) in model {
            let success = if success { "true" } else { "false" };
            self.model_calls
                .with_label_values(&[pass.as_str(), success])
                .set(count);
            self.model_duration_ms
                .with_label_values(&[&pass])
                .add(duration_ms);
        }
        let retries: i64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(GREATEST(attempt - 1, 0)), 0)::bigint FROM model_usage",
        )
        .fetch_one(pool)
        .await?;
        self.model_retries.set(retries);

        self.cleanup_pending.reset();
        let pending_documents: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM documents WHERE storage_path IS NOT NULL AND input_expires_at < now()",
        )
        .fetch_one(pool)
        .await?;
        let pending_reports: i64 =
            sqlx::query_scalar("SELECT count(*) FROM reports WHERE expires_at < now()")
                .fetch_one(pool)
                .await?;
        self.cleanup_pending
            .with_label_values(&["document"])
            .set(pending_documents);
        self.cleanup_pending
            .with_label_values(&["report"])
            .set(pending_reports);
        Ok(())
    }

    pub fn encode(&self) -> CoreResult<Vec<u8>> {
        let families = self.registry.gather();
        let mut output = Vec::new();
        TextEncoder::new()
            .encode(&families, &mut output)
            .map_err(|error| textcomb_core::CoreError::Internal(anyhow::Error::new(error)))?;
        Ok(output)
    }
}
