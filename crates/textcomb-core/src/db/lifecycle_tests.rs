use super::*;
use textcomb_domain::{
    ANALYZER_VERSION, AnalysisProfile, AnalysisSnapshot, DocumentFormat, DocumentMetadata,
};

struct Fixture {
    user_id: Uuid,
    job_id: Uuid,
    document_id: Uuid,
    profile_id: Uuid,
}

impl Fixture {
    async fn create(pool: &PgPool) -> Self {
        let fixture = Self {
            user_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            document_id: Uuid::new_v4(),
            profile_id: Uuid::new_v4(),
        };
        let profile_id = fixture.profile_id;
        sqlx::query(
            "INSERT INTO users(id, username, password_hash, role) VALUES($1,$2,'unused','user')",
        )
        .bind(fixture.user_id)
        .bind(format!("lifecycle-{}", fixture.user_id))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO model_profiles(id, owner_id, name, base_url, api_key_ciphertext, candidate_model, verifier_model, disclosure_accepted_at) VALUES($1,$2,'test','https://example.test',decode('00','hex'),'test','test',now())")
            .bind(profile_id).bind(fixture.user_id).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO documents(id, user_id, original_name, media_type, document_format, size_bytes, storage_path, extracted_text, input_expires_at) VALUES($1,$2,'test.txt','text/plain','txt',15,$3,'待分析正文',now() + interval '1 day')")
            .bind(fixture.document_id).bind(fixture.user_id)
            .bind(format!("/tmp/{}.txt", fixture.document_id)).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO analysis_jobs(id, user_id, document_id, model_profile_id, status, worker_id, analyzer_version, timeout_at, lease_until, model_snapshot) VALUES($1,$2,$3,$4,'rendering','new-worker','test',now() + interval '1 hour',now() + interval '2 minutes','{}')")
            .bind(fixture.job_id).bind(fixture.user_id).bind(fixture.document_id)
            .bind(profile_id).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO analysis_chunks(id, job_id, chunk_index, source_start, source_end, content, status, candidate_output, verifier_output) VALUES($1,$2,0,0,5,'待分析正文','completed','[]','[]')")
            .bind(Uuid::new_v4()).bind(fixture.job_id).execute(pool).await.unwrap();
        fixture
    }

    fn report(&self) -> ReportV1 {
        ReportV1::new(
            Uuid::new_v4(),
            self.job_id,
            DocumentMetadata {
                original_name: "test.txt".to_owned(),
                format: DocumentFormat::Txt,
                char_count: 5,
            },
            AnalysisSnapshot {
                analysis_profile: AnalysisProfile::General,
                provider_kind: "openai_compatible".to_owned(),
                candidate_model: "test".to_owned(),
                verifier_model: "test".to_owned(),
                prompt_version: "test".to_owned(),
                reference_version: "sha256:test".to_owned(),
                analyzer_version: ANALYZER_VERSION.to_owned(),
                reference_profile: false,
            },
            vec![],
            OffsetDateTime::now_utc(),
        )
    }

    async fn assert_input_intact(&self, pool: &PgPool) {
        let body: Option<String> =
            sqlx::query_scalar("SELECT extracted_text FROM documents WHERE id = $1")
                .bind(self.document_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let chunk: (Option<String>, Option<Value>, Option<Value>) = sqlx::query_as("SELECT content, candidate_output, verifier_output FROM analysis_chunks WHERE job_id = $1")
            .bind(self.job_id).fetch_one(pool).await.unwrap();
        assert_eq!(body.as_deref(), Some("待分析正文"));
        assert_eq!(chunk.0.as_deref(), Some("待分析正文"));
        assert!(chunk.1.is_some() && chunk.2.is_some());
    }

    async fn cleanup(&self, pool: &PgPool) {
        sqlx::query("DELETE FROM analysis_jobs WHERE id = $1")
            .bind(self.job_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(self.user_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn queued_deadlines_expire_without_claiming_or_model_usage() {
    let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let expired = Fixture::create(&pool).await;
    sqlx::query("UPDATE analysis_jobs SET status = 'queued', worker_id = NULL, lease_until = NULL, timeout_at = now() - interval '1 second' WHERE id = $1")
        .bind(expired.job_id).execute(&pool).await.unwrap();
    assert!(claim_job(&pool, "queue-worker").await.unwrap().is_none());
    let row: (String, Option<String>, i32, Option<OffsetDateTime>) = sqlx::query_as(
        "SELECT status, error_code, attempt_count, started_at FROM analysis_jobs WHERE id = $1",
    )
    .bind(expired.job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        ("failed".to_owned(), Some("JOB_TIMEOUT".to_owned()), 0, None)
    );
    let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM model_usage WHERE job_id = $1")
        .bind(expired.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(calls, 0);

    let waiting = Fixture::create(&pool).await;
    sqlx::query("UPDATE analysis_jobs SET status = 'queued', worker_id = NULL, lease_until = NULL, timeout_at = now() + interval '1 hour' WHERE id = $1")
        .bind(waiting.job_id).execute(&pool).await.unwrap();
    let claimed = claim_job(&pool, "queue-worker").await.unwrap().unwrap();
    assert_eq!(claimed.id, waiting.job_id);
    assert!(claimed.timeout_at > OffsetDateTime::now_utc());
    expired.cleanup(&pool).await;
    waiting.cleanup(&pool).await;
}

#[tokio::test]
async fn cached_model_limit_follows_live_updates_and_drains_existing_calls() {
    use crate::model_limits::ProfileLimit;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let fixture = Fixture::create(&pool).await;
    sqlx::query("UPDATE model_profiles SET max_concurrency = 2 WHERE id = $1")
        .bind(fixture.profile_id)
        .execute(&pool)
        .await
        .unwrap();
    let gate = Arc::new(ProfileLimit::new(
        pool.clone(),
        fixture.profile_id,
        fixture.user_id,
    ));
    let stop = CancellationToken::new();
    let first = gate.acquire(&stop).await.unwrap();
    let second = gate.acquire(&stop).await.unwrap();
    sqlx::query("UPDATE model_profiles SET max_concurrency = 1 WHERE id = $1")
        .bind(fixture.profile_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), gate.acquire(&stop))
            .await
            .is_err()
    );
    drop(second);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), gate.acquire(&stop))
            .await
            .is_err()
    );
    drop(first);
    let only = gate.acquire(&stop).await.unwrap();
    // A waiter already blocked on the cached gate must notice an increase.
    let waiting = gate.acquire(&stop);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    sqlx::query("UPDATE model_profiles SET max_concurrency = 2 WHERE id = $1")
        .bind(fixture.profile_id)
        .execute(&pool)
        .await
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(3), &mut waiting)
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    assert_eq!(
        gate.acquire(&stop).await.err().unwrap().code(),
        ErrorCode::Conflict
    );
    drop(next);
    drop(only);
    fixture.cleanup(&pool).await;
}

#[tokio::test]
async fn model_limit_rechecks_decreases_after_waiting_for_a_global_slot() {
    use crate::model_limits::ProfileLimit;
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;
    let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let fixture = Fixture::create(&pool).await;
    sqlx::query("UPDATE model_profiles SET max_concurrency = 2 WHERE id = $1")
        .bind(fixture.profile_id)
        .execute(&pool)
        .await
        .unwrap();
    let gate = Arc::new(ProfileLimit::new(
        pool.clone(),
        fixture.profile_id,
        fixture.user_id,
    ));
    let global = Arc::new(Semaphore::new(0));
    let stop = CancellationToken::new();
    let in_flight = gate.acquire(&stop).await.unwrap();
    let pending = gate.acquire_for_call(&global, &stop);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    sqlx::query("UPDATE model_profiles SET max_concurrency = 1 WHERE id = $1")
        .bind(fixture.profile_id)
        .execute(&pool)
        .await
        .unwrap();
    global.add_permits(1);
    // Opening the global slot must not bypass the lowered profile limit.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    assert_eq!(global.available_permits(), 1);
    drop(in_flight);
    let permits = tokio::time::timeout(Duration::from_secs(3), &mut pending)
        .await
        .unwrap()
        .unwrap();
    drop(permits);
    fixture.cleanup(&pool).await;
}

#[tokio::test]
async fn stale_worker_cannot_cancel_or_publish_reclaimed_job() {
    let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let fixture = Fixture::create(&pool).await;
    assert_eq!(
        finalize_cancelled(&pool, fixture.job_id, "old-worker")
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    let report = fixture.report();
    let expiry = OffsetDateTime::now_utc() + time::Duration::days(1);
    assert_eq!(
        save_report(
            &pool,
            "old-worker",
            &report,
            fixture.user_id,
            "",
            None,
            expiry
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::Conflict
    );
    fixture.assert_input_intact(&pool).await;
    let reports: i64 = sqlx::query_scalar("SELECT count(*) FROM reports WHERE job_id = $1")
        .bind(fixture.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(reports, 0);

    save_report(
        &pool,
        "new-worker",
        &report,
        fixture.user_id,
        "",
        None,
        expiry,
    )
    .await
    .unwrap();
    // Even the former owner's late cancellation must not change a completed report.
    assert_eq!(
        finalize_cancelled(&pool, fixture.job_id, "new-worker")
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    let status: String = sqlx::query_scalar("SELECT status FROM analysis_jobs WHERE id = $1")
        .bind(fixture.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");
    assert_eq!(
        load_report(&pool, report.report_id, fixture.user_id)
            .await
            .unwrap()
            .job_id,
        fixture.job_id
    );
    fixture.cleanup(&pool).await;
}

#[tokio::test]
async fn cancellation_requires_current_owner_and_requested_status() {
    let Ok(url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let fixture = Fixture::create(&pool).await;
    assert_eq!(
        finalize_cancelled(&pool, fixture.job_id, "new-worker")
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    fixture.assert_input_intact(&pool).await;
    request_cancellation(&pool, fixture.job_id, fixture.user_id)
        .await
        .unwrap();
    assert_eq!(
        finalize_cancelled(&pool, fixture.job_id, "old-worker")
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    let report = fixture.report();
    assert_eq!(
        save_report(
            &pool,
            "new-worker",
            &report,
            fixture.user_id,
            "",
            None,
            OffsetDateTime::now_utc() + time::Duration::days(1)
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::JobCancelled
    );
    fixture.assert_input_intact(&pool).await;
    assert!(
        finalize_cancelled(&pool, fixture.job_id, "new-worker")
            .await
            .unwrap()
            .is_some()
    );
    let row: (String, Option<Value>, Option<String>) = sqlx::query_as("SELECT job.status, job.model_snapshot, document.extracted_text FROM analysis_jobs AS job JOIN documents AS document ON document.id = job.document_id WHERE job.id = $1")
        .bind(fixture.job_id).fetch_one(&pool).await.unwrap();
    assert_eq!(row, ("cancelled".to_owned(), None, None));
    fixture.cleanup(&pool).await;
}
