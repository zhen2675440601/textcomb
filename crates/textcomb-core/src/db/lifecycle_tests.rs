use super::*;
use textcomb_domain::{
    ANALYZER_VERSION, AnalysisProfile, AnalysisSnapshot, DocumentFormat, DocumentMetadata,
};

struct Fixture {
    user_id: Uuid,
    job_id: Uuid,
    document_id: Uuid,
}

impl Fixture {
    async fn create(pool: &PgPool) -> Self {
        let fixture = Self {
            user_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            document_id: Uuid::new_v4(),
        };
        let profile_id = Uuid::new_v4();
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
