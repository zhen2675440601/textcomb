use crate::{
    error::{CoreError, CoreResult, ErrorCode},
    prompts,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Client, StatusCode, header::RETRY_AFTER};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use textcomb_domain::{GrammarSubtype, IssueCategory};
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct ProviderProfile {
    pub base_url: String,
    pub api_key: SecretString,
    pub candidate_model: String,
    pub verifier_model: String,
    pub candidate_system_prompt: String,
    pub verifier_system_prompt: String,
}

const MAX_MODEL_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CANDIDATES_PER_CHUNK: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateIssue {
    pub category: IssueCategory,
    #[serde(default)]
    pub grammar_subtype: Option<GrammarSubtype>,
    pub quote: String,
    #[serde(default)]
    pub context_before: Option<String>,
    #[serde(default)]
    pub context_after: Option<String>,
    pub reason: String,
    pub suggestion: String,
    pub confidence: u8,
    #[serde(default)]
    pub evidence_source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateResponse {
    pub issues: Vec<CandidateIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerdict {
    Confirmed,
    Suspected,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCandidate {
    pub candidate_index: usize,
    pub verdict: VerificationVerdict,
    pub confidence: u8,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub suggestion: String,
    #[serde(default)]
    pub evidence_source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationResponse {
    pub verdicts: Vec<VerifiedCandidate>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ProviderResult<T> {
    pub value: T,
    pub usage: TokenUsage,
    pub duration_ms: i64,
    pub attempts: i32,
    pub repair: Option<RepairUsage>,
}

#[derive(Debug, Clone)]
pub struct RepairUsage {
    pub usage: TokenUsage,
    pub duration_ms: i64,
}

#[async_trait]
pub trait AnalysisProvider: Send + Sync {
    async fn candidates(
        &self,
        text: &str,
        evidence_ids: &[String],
    ) -> CoreResult<ProviderResult<CandidateResponse>>;

    async fn verify(
        &self,
        text: &str,
        candidates: &[CandidateIssue],
        evidence_ids: &[String],
    ) -> CoreResult<ProviderResult<VerificationResponse>>;

    async fn test_connection(&self) -> CoreResult<()>;
}

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    client: Client,
    profile: ProviderProfile,
    endpoint: String,
}

impl OpenAiCompatibleProvider {
    pub fn new(profile: ProviderProfile) -> CoreResult<Self> {
        let base = profile.base_url.trim_end_matches('/');
        let endpoint = if base.ends_with("/chat/completions") {
            base.to_owned()
        } else {
            format!("{base}/chat/completions")
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("textcomb/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?;
        Ok(Self {
            client,
            profile,
            endpoint,
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> CoreResult<ProviderResult<T>> {
        let started = Instant::now();
        let request = ChatRequest {
            model,
            temperature: 0.0,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            response_format: ResponseFormat {
                kind: "json_object",
            },
        };

        let mut last_public_message = "模型服务暂时不可用".to_owned();
        for attempt in 1..=3_i32 {
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(self.profile.api_key.expose_secret())
                .json(&request)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let response = read_chat_response(response).await?;
                    let content = response
                        .choices
                        .first()
                        .map(|choice| choice.message.content.as_str())
                        .filter(|content| !content.trim().is_empty())
                        .ok_or_else(|| {
                            CoreError::public(
                                ErrorCode::ModelOutputInvalid,
                                "模型没有返回结构化内容",
                            )
                        })?;
                    let usage = TokenUsage {
                        prompt_tokens: response
                            .usage
                            .as_ref()
                            .and_then(|usage| usage.prompt_tokens),
                        completion_tokens: response
                            .usage
                            .as_ref()
                            .and_then(|usage| usage.completion_tokens),
                    };
                    let request_duration_ms = started.elapsed().as_millis() as i64;
                    let (value, repair) = match parse_json_content(content) {
                        Ok(value) => (value, None),
                        Err(_) => {
                            let (value, repair) = self.repair_json(model, content).await?;
                            (value, Some(repair))
                        }
                    };
                    return Ok(ProviderResult {
                        value,
                        usage,
                        duration_ms: request_duration_ms,
                        attempts: attempt,
                        repair,
                    });
                }
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after(&response);
                    last_public_message = if status == StatusCode::TOO_MANY_REQUESTS {
                        "模型服务触发限流".to_owned()
                    } else {
                        format!("模型服务返回 HTTP {}", status.as_u16())
                    };
                    if !is_retryable(status) || attempt == 3 {
                        break;
                    }
                    sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
                }
                Err(error) => {
                    last_public_message = if error.is_timeout() {
                        "模型服务响应超时".to_owned()
                    } else {
                        "无法连接模型服务".to_owned()
                    };
                    if attempt == 3 {
                        break;
                    }
                    sleep(backoff(attempt)).await;
                }
            }
        }
        Err(CoreError::public(
            ErrorCode::ProviderUnavailable,
            last_public_message,
        ))
    }

    async fn repair_json<T: DeserializeOwned>(
        &self,
        model: &str,
        invalid_content: &str,
    ) -> CoreResult<(T, RepairUsage)> {
        let user = format!(
            "把下面内容修复成语义等价且符合原约定结构的 JSON 对象。只能输出 JSON，不能新增判断：\n<invalid>\n{invalid_content}\n</invalid>"
        );
        let request = ChatRequest {
            model,
            temperature: 0.0,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: "你是严格的 JSON 结构修复器，只修复语法和外层格式。",
                },
                ChatMessage {
                    role: "user",
                    content: &user,
                },
            ],
            response_format: ResponseFormat {
                kind: "json_object",
            },
        };
        let started = Instant::now();
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.profile.api_key.expose_secret())
            .json(&request)
            .send()
            .await
            .map_err(|_| {
                CoreError::public(ErrorCode::ModelOutputInvalid, "模型 JSON 修复请求失败")
            })?;
        if !response.status().is_success() {
            return Err(CoreError::public(
                ErrorCode::ModelOutputInvalid,
                "模型 JSON 修复请求未成功",
            ));
        }
        let response = read_chat_response(response).await?;
        let content = response
            .choices
            .first()
            .map(|choice| choice.message.content.as_str())
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| {
                CoreError::public(ErrorCode::ModelOutputInvalid, "模型 JSON 修复结果为空")
            })?;
        let value = parse_json_content(content)?;
        Ok((
            value,
            RepairUsage {
                usage: TokenUsage {
                    prompt_tokens: response
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.prompt_tokens),
                    completion_tokens: response
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.completion_tokens),
                },
                duration_ms: started.elapsed().as_millis() as i64,
            },
        ))
    }
}

#[async_trait]
impl AnalysisProvider for OpenAiCompatibleProvider {
    async fn candidates(
        &self,
        text: &str,
        evidence_ids: &[String],
    ) -> CoreResult<ProviderResult<CandidateResponse>> {
        let result = self
            .call(
                &self.profile.candidate_model,
                &self.profile.candidate_system_prompt,
                &prompts::candidate_user_prompt(text, evidence_ids),
            )
            .await?;
        validate_candidates(&result.value)?;
        Ok(result)
    }

    async fn verify(
        &self,
        text: &str,
        candidates: &[CandidateIssue],
        evidence_ids: &[String],
    ) -> CoreResult<ProviderResult<VerificationResponse>> {
        let candidate_count = candidates.len();
        let candidates_json = serde_json::to_string(candidates)?;
        let result = self
            .call(
                &self.profile.verifier_model,
                &self.profile.verifier_system_prompt,
                &prompts::verifier_user_prompt(text, &candidates_json, evidence_ids),
            )
            .await?;
        validate_verification(&result.value, candidate_count)?;
        Ok(result)
    }

    async fn test_connection(&self) -> CoreResult<()> {
        #[derive(Deserialize)]
        struct TestResponse {
            ok: bool,
        }
        let result: ProviderResult<TestResponse> = self
            .call(
                &self.profile.candidate_model,
                "只返回 JSON 对象。",
                r#"返回 {"ok": true}"#,
            )
            .await?;
        if !result.value.ok {
            return Err(CoreError::public(
                ErrorCode::ProviderUnavailable,
                "模型连接测试没有返回预期结果",
            ));
        }
        Ok(())
    }
}

async fn read_chat_response(response: reqwest::Response) -> CoreResult<ChatResponse> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            CoreError::public(ErrorCode::ModelOutputInvalid, "模型服务响应读取失败")
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_MODEL_RESPONSE_BYTES {
            return Err(CoreError::public(
                ErrorCode::ModelOutputInvalid,
                "模型服务响应超过 1 MiB 安全上限",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| CoreError::public(ErrorCode::ModelOutputInvalid, "模型服务返回了无效响应"))
}

fn validate_candidates(response: &CandidateResponse) -> CoreResult<()> {
    if response.issues.len() > MAX_CANDIDATES_PER_CHUNK {
        return Err(invalid_output("单个文本块的候选问题超过 200 条安全上限"));
    }
    for issue in &response.issues {
        let subtype_is_valid =
            matches!(issue.category, IssueCategory::Grammar) == issue.grammar_subtype.is_some();
        let strings_are_valid = (1..=4_000).contains(&issue.quote.chars().count())
            && issue.reason.chars().count() <= 2_000
            && issue.suggestion.chars().count() <= 2_000
            && issue
                .context_before
                .as_deref()
                .is_none_or(|value| value.chars().count() <= 500)
            && issue
                .context_after
                .as_deref()
                .is_none_or(|value| value.chars().count() <= 500);
        let evidence_is_valid = issue.evidence_source_ids.len() <= 16
            && issue
                .evidence_source_ids
                .iter()
                .all(|source_id| source_id.chars().count() <= 80);
        if issue.confidence > 100 || !subtype_is_valid || !strings_are_valid || !evidence_is_valid {
            return Err(invalid_output("候选问题不符合字段约束"));
        }
    }
    Ok(())
}

fn validate_verification(
    response: &VerificationResponse,
    candidate_count: usize,
) -> CoreResult<()> {
    if response.verdicts.len() != candidate_count {
        return Err(invalid_output("复核结果必须逐项覆盖所有候选问题"));
    }
    let mut indexes = HashSet::with_capacity(response.verdicts.len());
    for verdict in &response.verdicts {
        let evidence_is_valid = verdict.evidence_source_ids.len() <= 16
            && verdict
                .evidence_source_ids
                .iter()
                .all(|source_id| source_id.chars().count() <= 80);
        if verdict.candidate_index >= candidate_count
            || !indexes.insert(verdict.candidate_index)
            || verdict.confidence > 100
            || verdict.reason.chars().count() > 2_000
            || verdict.suggestion.chars().count() > 2_000
            || !evidence_is_valid
        {
            return Err(invalid_output("复核结果不符合字段约束"));
        }
    }
    Ok(())
}

fn invalid_output(message: &'static str) -> CoreError {
    CoreError::public(ErrorCode::ModelOutputInvalid, message)
}

fn parse_json_content<T: DeserializeOwned>(content: &str) -> CoreResult<T> {
    let trimmed = content.trim();
    let json = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    serde_json::from_str(json).map_err(|_| {
        CoreError::public(
            ErrorCode::ModelOutputInvalid,
            "模型返回内容不符合约定的 JSON 结构",
        )
    })
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(30)))
}

fn backoff(attempt: i32) -> Duration {
    Duration::from_secs(2_u64.saturating_pow((attempt - 1) as u32).min(8))
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    temperature: f32,
    messages: Vec<ChatMessage<'a>>,
    response_format: ResponseFormat<'a>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_category_and_subtype_mismatch() {
        let response = CandidateResponse {
            issues: vec![CandidateIssue {
                category: IssueCategory::Typo,
                grammar_subtype: Some(GrammarSubtype::WordOrder),
                quote: "错字".to_owned(),
                context_before: None,
                context_after: None,
                reason: "测试".to_owned(),
                suggestion: "措辞".to_owned(),
                confidence: 90,
                evidence_source_ids: vec![],
            }],
        };
        assert_eq!(
            validate_candidates(&response).unwrap_err().code(),
            ErrorCode::ModelOutputInvalid
        );
    }

    #[test]
    fn rejects_duplicate_verdict_indexes() {
        let verdict = VerifiedCandidate {
            candidate_index: 0,
            verdict: VerificationVerdict::Confirmed,
            confidence: 90,
            reason: "测试".to_owned(),
            suggestion: "建议".to_owned(),
            evidence_source_ids: vec![],
        };
        let response = VerificationResponse {
            verdicts: vec![verdict.clone(), verdict],
        };
        assert_eq!(
            validate_verification(&response, 2).unwrap_err().code(),
            ErrorCode::ModelOutputInvalid
        );
    }
}
