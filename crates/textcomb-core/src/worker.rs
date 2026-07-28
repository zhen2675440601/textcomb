use crate::{
    AppConfig,
    analysis::{self, AnalysisChunk},
    crypto::decrypt_secret,
    db, documents,
    error::{CoreError, CoreResult, ErrorCode},
    provider::{
        AnalysisProvider, CandidateIssue, ProviderKind, ProviderProfile, VerifiedCandidate,
        create_provider,
    },
    reporting,
    storage::LocalStorage,
};
use ::time::OffsetDateTime;
use futures::{StreamExt, stream};
use serde_json::json;
use sqlx::PgPool;
use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};
use textcomb_domain::{
    ANALYZER_VERSION, AnalysisSnapshot, DocumentFormat, DocumentMetadata, EvidenceRef, Issue,
    ReportV1,
};
use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinSet,
    time,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct Worker {
    pool: PgPool,
    config: Arc<AppConfig>,
    storage: LocalStorage,
    model_semaphore: Arc<Semaphore>,
    profile_semaphores: Arc<Mutex<HashMap<Uuid, Arc<Semaphore>>>>,
}

impl Worker {
    pub async fn new(pool: PgPool, config: Arc<AppConfig>) -> CoreResult<Self> {
        let storage = LocalStorage::new(config.storage_dir()).await?;
        let model_semaphore = Arc::new(Semaphore::new(config.model_concurrency));
        Ok(Self {
            pool,
            config,
            storage,
            model_semaphore,
            profile_semaphores: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn run(self) -> CoreResult<()> {
        db::recover_expired_leases(&self.pool).await?;
        let cleanup_worker = self.clone();
        tokio::spawn(async move {
            cleanup_worker.cleanup_loop().await;
        });

        let mut workers = JoinSet::new();
        for slot in 0..self.config.worker_concurrency {
            let worker = self.clone();
            workers.spawn(async move {
                let worker_id = format!("{}-{slot}", Uuid::new_v4());
                worker.worker_loop(worker_id).await
            });
        }
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => warn!("worker loop exited normally"),
                Ok(Err(error)) => error!(error = %error, "worker loop failed"),
                Err(error) => error!(error = %error, "worker task panicked"),
            }
        }
        Err(CoreError::public(
            ErrorCode::Internal,
            "所有 Worker 循环均已退出",
        ))
    }

    pub async fn run_once(&self, worker_id: &str) -> CoreResult<bool> {
        let Some(job) = db::claim_job(&self.pool, worker_id).await? else {
            return Ok(false);
        };
        info!(job_id = %job.id, worker_id, "claimed analysis job");
        self.process_claimed(job, worker_id).await;
        Ok(true)
    }

    async fn worker_loop(&self, worker_id: String) -> CoreResult<()> {
        loop {
            match self.run_once(&worker_id).await {
                Ok(true) => {}
                Ok(false) => time::sleep(Duration::from_millis(750)).await,
                Err(error) => {
                    error!(worker_id, error = %error, "worker polling failed");
                    time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn process_claimed(&self, job: db::ClaimedJob, worker_id: &str) {
        let heartbeat_cancel = CancellationToken::new();
        let heartbeat_task = {
            let pool = self.pool.clone();
            let job_id = job.id;
            let worker_id = worker_id.to_owned();
            let cancel = heartbeat_cancel.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            if let Err(error) = db::refresh_lease(&pool, job_id, &worker_id).await {
                                warn!(job_id = %job_id, error = %error, "job lease heartbeat failed");
                                break;
                            }
                        }
                    }
                }
            })
        };
        let result =
            time::timeout(self.config.job_timeout, self.process_job(&job, worker_id)).await;
        heartbeat_cancel.cancel();
        let _ = heartbeat_task.await;
        match result {
            Ok(Ok(())) => info!(job_id = %job.id, "analysis job completed"),
            Ok(Err(error)) if error.code() == ErrorCode::JobCancelled => {
                if let Err(mark_error) = self.cancel_and_purge(&job).await {
                    error!(job_id = %job.id, error = %mark_error, "failed to finalize cancellation");
                }
            }
            Ok(Err(error)) => {
                if self.finalize_if_cancelled(&job).await {
                    return;
                }
                error!(job_id = %job.id, code = error.code().as_str(), "analysis job failed");
                if let Err(mark_error) = db::mark_failed(&self.pool, job.id, &error).await {
                    error!(job_id = %job.id, error = %mark_error, "failed to mark job failed");
                }
            }
            Err(_) => {
                if self.finalize_if_cancelled(&job).await {
                    return;
                }
                let minutes = self.config.job_timeout.as_secs() / 60;
                let error = CoreError::public(
                    ErrorCode::JobTimeout,
                    format!("任务超过 {minutes} 分钟强制超时"),
                );
                if let Err(mark_error) = db::mark_failed(&self.pool, job.id, &error).await {
                    error!(job_id = %job.id, error = %mark_error, "failed to mark timeout");
                }
            }
        }
    }

    async fn process_job(&self, job: &db::ClaimedJob, worker_id: &str) -> CoreResult<()> {
        self.check_cancel(job.id).await?;
        let document_record = db::load_document(&self.pool, job.document_id).await?;
        let model_record =
            db::load_model_profile(&self.pool, job.model_profile_id, job.user_id).await?;
        let prompt_record = match job.prompt_version_id {
            Some(prompt_id) => db::load_prompt(&self.pool, prompt_id).await?,
            None => db::load_active_prompt(&self.pool).await?,
        };
        let evidence = Arc::new(db::load_evidence(&self.pool).await?);
        let storage_path = document_record
            .storage_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                CoreError::public(
                    ErrorCode::NotFound,
                    "原文件已删除，无法重新分析；请重新上传",
                )
            })?;
        let format = DocumentFormat::from_str(&document_record.document_format)
            .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?;
        let extracted = Arc::new(documents::extract(&storage_path, format).await?);
        let input_expiry = OffsetDateTime::now_utc()
            + ::time::Duration::hours(self.config.failed_input_retention_hours);
        db::save_extracted_document(
            &self.pool,
            document_record.id,
            &extracted.text,
            extracted.char_count,
            input_expiry,
        )
        .await?;

        let chunks = analysis::chunk_text(&extracted.text);
        analysis::require_non_empty_chunks(&chunks)?;
        db::replace_chunks(&self.pool, job.id, &chunks).await?;

        let model_snapshot = json!({
            "provider_kind": model_record.provider_kind.clone(),
            "candidate_model": model_record.candidate_model.clone(),
            "verifier_model": model_record.verifier_model.clone(),
            "prompt_version": prompt_record.version.clone(),
            "reference_profile": model_record.is_reference,
        });
        db::set_job_configuration(&self.pool, job.id, prompt_record.id, &model_snapshot).await?;

        let api_key = decrypt_secret(&self.config.master_key, &model_record.api_key_ciphertext)?;
        let provider_kind = ProviderKind::parse(&model_record.provider_kind)?;
        let provider = create_provider(
            provider_kind,
            ProviderProfile {
                base_url: model_record.base_url.clone(),
                api_key,
                candidate_model: model_record.candidate_model.clone(),
                verifier_model: model_record.verifier_model.clone(),
                candidate_system_prompt: prompt_record.candidate_template.clone(),
                verifier_system_prompt: prompt_record.verifier_template.clone(),
            },
        )?;
        let evidence_ids: Arc<Vec<String>> = Arc::new(evidence.keys().cloned().collect());
        let profile_semaphore = {
            let mut semaphores = self.profile_semaphores.lock().await;
            semaphores
                .entry(model_record.id)
                .or_insert_with(|| {
                    Arc::new(Semaphore::new(
                        usize::try_from(model_record.max_concurrency)
                            .unwrap_or(1)
                            .clamp(1, 100),
                    ))
                })
                .clone()
        };
        let mut all_issues = Vec::new();
        let total_chunks = chunks.len();
        let per_job = self.config.per_job_chunk_concurrency;
        let confirmed_threshold = prompt_record.confirmed_threshold.clamp(0, 100) as u8;
        let suspected_threshold = prompt_record.suspected_threshold.clamp(0, 100) as u8;
        let reusable: HashMap<i32, db::ReusableChunkOutput> =
            db::load_reusable_chunk_outputs(&self.pool, job.id)
                .await?
                .into_iter()
                .map(|output| (output.chunk_index, output))
                .collect();
        let mut pending_chunks = Vec::new();
        let mut completed = 0_usize;
        for chunk in chunks {
            let Some(output) = reusable.get(&(chunk.index as i32)) else {
                pending_chunks.push(chunk);
                continue;
            };
            let candidates =
                serde_json::from_value::<Vec<CandidateIssue>>(output.candidate_output.clone());
            let verdicts =
                serde_json::from_value::<Vec<VerifiedCandidate>>(output.verifier_output.clone());
            match (candidates, verdicts) {
                (Ok(candidates), Ok(verdicts)) => {
                    let resolved = analysis::resolve_candidates(&chunk, candidates);
                    let mut issues = analysis::finalize_issues(
                        &extracted,
                        resolved,
                        verdicts,
                        &evidence,
                        confirmed_threshold,
                        suspected_threshold,
                    )?;
                    all_issues.append(&mut issues);
                    completed += 1;
                }
                _ => {
                    warn!(job_id = %job.id, chunk_index = chunk.index, "stored chunk output was invalid; rerunning chunk");
                    pending_chunks.push(chunk);
                }
            }
        }
        let initial_progress = 10 + ((completed * 75 / total_chunks.max(1)) as i16);
        db::heartbeat(
            &self.pool,
            job.id,
            worker_id,
            "analyzing",
            "analyzing",
            initial_progress,
            completed as i32,
        )
        .await?;
        let job_id = job.id;

        let tasks = stream::iter(pending_chunks.into_iter().map(|chunk| {
            let provider = provider.clone();
            let extracted = extracted.clone();
            let evidence = evidence.clone();
            let evidence_ids = evidence_ids.clone();
            let pool = self.pool.clone();
            let semaphore = self.model_semaphore.clone();
            let profile_semaphore = profile_semaphore.clone();
            let candidate_model = model_record.candidate_model.clone();
            let verifier_model = model_record.verifier_model.clone();
            let provider_kind = model_record.provider_kind.clone();
            let worker = self.clone();
            async move {
                worker.check_cancel(job_id).await?;
                let chunk_index = chunk.index;
                db::mark_chunk_analyzing(&pool, job_id, chunk_index).await?;
                let result = analyze_chunk(
                    &pool,
                    job_id,
                    chunk,
                    provider,
                    semaphore,
                    profile_semaphore,
                    extracted,
                    evidence,
                    evidence_ids,
                    &provider_kind,
                    &candidate_model,
                    &verifier_model,
                    confirmed_threshold,
                    suspected_threshold,
                )
                .await;
                if let Err(error) = &result
                    && let Err(mark_error) =
                        db::mark_chunk_failed(&pool, job_id, chunk_index, error).await
                {
                    warn!(job_id = %job_id, chunk_index, error = %mark_error, "failed to mark chunk failed");
                }
                result
            }
        }))
        .buffer_unordered(per_job);
        tokio::pin!(tasks);
        while let Some(result) = tasks.next().await {
            let mut issues = result?;
            all_issues.append(&mut issues);
            completed += 1;
            let progress = 10 + ((completed * 75 / total_chunks.max(1)) as i16);
            db::heartbeat(
                &self.pool,
                job.id,
                worker_id,
                "verifying",
                "verifying",
                progress,
                completed as i32,
            )
            .await?;
        }
        self.check_cancel(job.id).await?;
        db::heartbeat(
            &self.pool,
            job.id,
            worker_id,
            "merging",
            "merging",
            87,
            completed as i32,
        )
        .await?;
        let issues = analysis::deduplicate_issues(all_issues);
        db::heartbeat(
            &self.pool,
            job.id,
            worker_id,
            "rendering",
            "rendering",
            90,
            completed as i32,
        )
        .await?;

        let report_id = Uuid::new_v4();
        let report = ReportV1::new(
            report_id,
            job.id,
            DocumentMetadata {
                original_name: document_record.original_name.clone(),
                format,
                char_count: extracted.char_count as u32,
            },
            AnalysisSnapshot {
                provider_kind: model_record.provider_kind.clone(),
                candidate_model: model_record.candidate_model.clone(),
                verifier_model: model_record.verifier_model.clone(),
                prompt_version: prompt_record.version.clone(),
                analyzer_version: ANALYZER_VERSION.to_owned(),
                reference_profile: model_record.is_reference,
            },
            issues,
            OffsetDateTime::now_utc(),
        );
        let markdown = reporting::to_markdown(&report);
        let pdf_path = self.storage.report_pdf_path(report_id);
        let typst_path = self.storage.temporary_path(report_id, "typ");
        self.check_cancel(job.id).await?;
        reporting::render_pdf(&report, &pdf_path, &typst_path).await?;
        if let Err(error) = self.check_cancel(job.id).await {
            if let Err(remove_error) = self.storage.remove(&pdf_path).await {
                warn!(path = %pdf_path.display(), error = %remove_error, "failed to remove cancelled report PDF");
            }
            return Err(error);
        }
        let report_expiry =
            OffsetDateTime::now_utc() + ::time::Duration::days(self.config.report_retention_days);
        let save_result = db::save_report(
            &self.pool,
            &report,
            job.user_id,
            &markdown,
            pdf_path.to_str(),
            report_expiry,
        )
        .await;
        if let Err(error) = save_result {
            if let Err(remove_error) = self.storage.remove(&pdf_path).await {
                warn!(path = %pdf_path.display(), error = %remove_error, "failed to remove uncommitted report PDF");
            }
            return Err(error);
        }
        self.remove_finalized_input(&storage_path).await;
        Ok(())
    }

    async fn check_cancel(&self, job_id: Uuid) -> CoreResult<()> {
        if db::is_cancel_requested(&self.pool, job_id).await? {
            return Err(CoreError::public(
                ErrorCode::JobCancelled,
                "任务已由用户取消",
            ));
        }
        Ok(())
    }

    async fn cancel_and_purge(&self, job: &db::ClaimedJob) -> CoreResult<()> {
        if let Some(path) = db::finalize_cancelled(&self.pool, job.id).await? {
            self.remove_finalized_input(&path).await;
        }
        Ok(())
    }

    async fn finalize_if_cancelled(&self, job: &db::ClaimedJob) -> bool {
        match db::is_cancel_requested(&self.pool, job.id).await {
            Ok(true) => {
                if let Err(error) = self.cancel_and_purge(job).await {
                    error!(job_id = %job.id, error = %error, "failed to finalize cancellation");
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                error!(job_id = %job.id, error = %error, "failed to read cancellation state");
                false
            }
        }
    }

    async fn remove_finalized_input(&self, path: &std::path::Path) {
        match self.storage.remove(path).await {
            Ok(()) => {
                if let Err(error) = db::confirm_document_file_removed(&self.pool, path).await {
                    warn!(path = %path.display(), error = %error, "input file was removed but its cleanup marker remains; cleanup will retry");
                }
            }
            Err(error) => {
                warn!(path = %path.display(), error = %error, "failed to remove finalized input file; cleanup will retry");
            }
        }
    }

    async fn cleanup_loop(&self) {
        loop {
            if let Err(error) = self.cleanup().await {
                error!(error = %error, "scheduled cleanup failed");
            }
            time::sleep(Duration::from_secs(3600)).await;
        }
    }

    async fn cleanup(&self) -> CoreResult<()> {
        db::recover_expired_leases(&self.pool).await?;
        crate::auth::cleanup_sessions(&self.pool).await?;
        for target in db::cleanup_expired(&self.pool).await? {
            let path = match &target {
                db::CleanupTarget::Document(path) => Some(path),
                db::CleanupTarget::Report { path, .. } => path.as_ref(),
            };
            let remove_result = match path {
                Some(path) => self.storage.remove(path).await,
                None => Ok(()),
            };
            match remove_result {
                Ok(()) => db::confirm_cleanup_target(&self.pool, &target).await?,
                Err(error) => {
                    warn!(path = path.map(|value| value.display().to_string()), error = %error, "failed to remove expired file; cleanup will retry");
                }
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn analyze_chunk(
    pool: &PgPool,
    job_id: Uuid,
    chunk: AnalysisChunk,
    provider: Arc<dyn AnalysisProvider>,
    semaphore: Arc<Semaphore>,
    profile_semaphore: Arc<Semaphore>,
    extracted: Arc<documents::ExtractedDocument>,
    evidence: Arc<std::collections::HashMap<String, EvidenceRef>>,
    evidence_ids: Arc<Vec<String>>,
    provider_kind: &str,
    candidate_model: &str,
    verifier_model: &str,
    confirmed_threshold: u8,
    suspected_threshold: u8,
) -> CoreResult<Vec<Issue>> {
    let candidate_result = {
        let _profile_permit = profile_semaphore.acquire().await.map_err(|_| {
            CoreError::public(ErrorCode::ProviderUnavailable, "模型配置并发控制器已关闭")
        })?;
        let _permit = semaphore.acquire().await.map_err(|_| {
            CoreError::public(ErrorCode::ProviderUnavailable, "模型并发控制器已关闭")
        })?;
        ensure_not_cancelled(pool, job_id).await?;
        provider.candidates(&chunk.text, &evidence_ids).await?
    };
    db::record_usage(
        pool,
        db::UsageRecord {
            job_id,
            chunk_id: None,
            pass: "candidate",
            provider_kind,
            model: candidate_model,
            usage: &candidate_result.usage,
            duration_ms: candidate_result.duration_ms,
            attempt: candidate_result.attempts,
            success: true,
            error_code: None,
        },
    )
    .await?;
    if let Some(repair) = &candidate_result.repair {
        db::record_usage(
            pool,
            db::UsageRecord {
                job_id,
                chunk_id: None,
                pass: "repair",
                provider_kind,
                model: candidate_model,
                usage: &repair.usage,
                duration_ms: repair.duration_ms,
                attempt: 1,
                success: true,
                error_code: None,
            },
        )
        .await?;
    }
    ensure_not_cancelled(pool, job_id).await?;
    let candidates: Vec<CandidateIssue> = candidate_result.value.issues;
    let resolved = analysis::resolve_candidates(&chunk, candidates.clone());
    db::mark_chunk_verifying(pool, job_id, chunk.index).await?;
    let verification_result = {
        let _profile_permit = profile_semaphore.acquire().await.map_err(|_| {
            CoreError::public(ErrorCode::ProviderUnavailable, "模型配置并发控制器已关闭")
        })?;
        let _permit = semaphore.acquire().await.map_err(|_| {
            CoreError::public(ErrorCode::ProviderUnavailable, "模型并发控制器已关闭")
        })?;
        ensure_not_cancelled(pool, job_id).await?;
        provider
            .verify(&chunk.text, &candidates, &evidence_ids)
            .await?
    };
    db::record_usage(
        pool,
        db::UsageRecord {
            job_id,
            chunk_id: None,
            pass: "verification",
            provider_kind,
            model: verifier_model,
            usage: &verification_result.usage,
            duration_ms: verification_result.duration_ms,
            attempt: verification_result.attempts,
            success: true,
            error_code: None,
        },
    )
    .await?;
    if let Some(repair) = &verification_result.repair {
        db::record_usage(
            pool,
            db::UsageRecord {
                job_id,
                chunk_id: None,
                pass: "repair",
                provider_kind,
                model: verifier_model,
                usage: &repair.usage,
                duration_ms: repair.duration_ms,
                attempt: 1,
                success: true,
                error_code: None,
            },
        )
        .await?;
    }
    let candidate_json = serde_json::to_value(&candidates)?;
    let verifier_json = serde_json::to_value(&verification_result.value.verdicts)?;
    db::save_chunk_outputs(pool, job_id, chunk.index, &candidate_json, &verifier_json).await?;
    analysis::finalize_issues(
        &extracted,
        resolved,
        verification_result.value.verdicts,
        &evidence,
        confirmed_threshold,
        suspected_threshold,
    )
}

async fn ensure_not_cancelled(pool: &PgPool, job_id: Uuid) -> CoreResult<()> {
    if db::is_cancel_requested(pool, job_id).await? {
        return Err(CoreError::public(
            ErrorCode::JobCancelled,
            "任务已由用户取消",
        ));
    }
    Ok(())
}
