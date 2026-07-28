use crate::{
    error::{CoreError, CoreResult, ErrorCode},
    prompts,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode, header::RETRY_AFTER};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use textcomb_domain::{GrammarSubtype, IssueCategory};
use tokio::time::sleep;

const MAX_MODEL_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CANDIDATES_PER_CHUNK: usize = 200;
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const ANTHROPIC_MAX_OUTPUT_TOKENS: u32 = 8_192;

/// The wire protocol selected for a model profile.
///
/// The stored representation is deliberately stable because it is included in
/// report snapshots and model-usage records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAiResponses,
    #[default]
    OpenAiCompatible,
    Anthropic,
}

impl ProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(value: &str) -> CoreResult<Self> {
        match value.trim() {
            "openai_responses" => Ok(Self::OpenAiResponses),
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "不支持的接口格式；可选值为 openai_responses、openai_compatible、anthropic",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderProfile {
    pub base_url: String,
    pub api_key: SecretString,
    pub candidate_model: String,
    pub verifier_model: String,
    pub candidate_system_prompt: String,
    pub verifier_system_prompt: String,
}

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

/// Constructs the selected protocol implementation while keeping all analysis
/// callers independent from its transport details.
pub fn create_provider(
    provider_kind: ProviderKind,
    profile: ProviderProfile,
) -> CoreResult<Arc<dyn AnalysisProvider>> {
    Ok(Arc::new(JsonProtocolProvider::new(provider_kind, profile)?))
}

#[derive(Clone)]
struct JsonProtocolProvider {
    client: Client,
    profile: ProviderProfile,
    endpoint: String,
    protocol: ProviderKind,
}

impl JsonProtocolProvider {
    fn new(protocol: ProviderKind, profile: ProviderProfile) -> CoreResult<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("textcomb/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?;
        let endpoint = endpoint_for(&profile.base_url, protocol);
        Ok(Self {
            client,
            profile,
            endpoint,
            protocol,
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> CoreResult<ProviderResult<T>> {
        let raw = self.request_content(model, system, user).await?;
        let (value, repair) = match parse_json_content(&raw.content) {
            Ok(value) => (value, None),
            Err(_) => {
                let (value, repair) = self.repair_json(model, &raw.content).await?;
                (value, Some(repair))
            }
        };
        Ok(ProviderResult {
            value,
            usage: raw.usage,
            duration_ms: raw.duration_ms,
            attempts: raw.attempts,
            repair,
        })
    }

    async fn request_content(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> CoreResult<RawProviderResult> {
        let started = Instant::now();
        let request = self.request_body(model, system, user);
        let mut last_public_message = "模型服务暂时不可用".to_owned();

        for attempt in 1..=3_i32 {
            let response = self.authenticated_request(&request).send().await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let body = read_model_response(response).await?;
                    let (content, usage) = extract_provider_output(self.protocol, body)?;
                    return Ok(RawProviderResult {
                        content,
                        usage,
                        duration_ms: started.elapsed().as_millis() as i64,
                        attempts: attempt,
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
        let raw = self
            .request_content(
                model,
                "你是严格的 JSON 结构修复器，只修复语法和外层格式。",
                &user,
            )
            .await
            .map_err(|_| {
                CoreError::public(ErrorCode::ModelOutputInvalid, "模型 JSON 修复请求失败")
            })?;
        let value = parse_json_content(&raw.content)?;
        Ok((
            value,
            RepairUsage {
                usage: raw.usage,
                duration_ms: raw.duration_ms,
            },
        ))
    }

    fn request_body(&self, model: &str, system: &str, user: &str) -> Value {
        match self.protocol {
            ProviderKind::OpenAiCompatible => json!({
                "model": model,
                "temperature": 0.0,
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user }
                ],
                "response_format": { "type": "json_object" }
            }),
            ProviderKind::OpenAiResponses => json!({
                "model": model,
                "instructions": system,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": user }]
                }],
                // Responses are retained by default by some providers. TextComb
                // processes documents only for the current request, so opt out.
                "store": false,
                "text": { "format": { "type": "json_object" } }
            }),
            ProviderKind::Anthropic => json!({
                "model": model,
                "max_tokens": ANTHROPIC_MAX_OUTPUT_TOKENS,
                "temperature": 0.0,
                "system": system,
                "messages": [{ "role": "user", "content": user }]
            }),
        }
    }

    fn authenticated_request(&self, request: &Value) -> RequestBuilder {
        let builder = self.client.post(&self.endpoint).json(request);
        match self.protocol {
            ProviderKind::OpenAiResponses | ProviderKind::OpenAiCompatible => {
                builder.bearer_auth(self.profile.api_key.expose_secret())
            }
            ProviderKind::Anthropic => builder
                .header("x-api-key", self.profile.api_key.expose_secret())
                .header("anthropic-version", ANTHROPIC_API_VERSION),
        }
    }
}

#[async_trait]
impl AnalysisProvider for JsonProtocolProvider {
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

#[derive(Debug)]
struct RawProviderResult {
    content: String,
    usage: TokenUsage,
    duration_ms: i64,
    attempts: i32,
}

fn endpoint_for(base_url: &str, protocol: ProviderKind) -> String {
    let base = base_url.trim_end_matches('/');
    let suffix = match protocol {
        ProviderKind::OpenAiResponses => "/responses",
        ProviderKind::OpenAiCompatible => "/chat/completions",
        ProviderKind::Anthropic => "/messages",
    };
    if base.ends_with(suffix) {
        base.to_owned()
    } else {
        format!("{base}{suffix}")
    }
}

async fn read_model_response(response: reqwest::Response) -> CoreResult<Value> {
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

fn extract_provider_output(
    protocol: ProviderKind,
    body: Value,
) -> CoreResult<(String, TokenUsage)> {
    match protocol {
        ProviderKind::OpenAiCompatible => extract_chat_output(body),
        ProviderKind::OpenAiResponses => extract_responses_output(body),
        ProviderKind::Anthropic => extract_anthropic_output(body),
    }
}

fn extract_chat_output(body: Value) -> CoreResult<(String, TokenUsage)> {
    let response: ChatResponse = serde_json::from_value(body)
        .map_err(|_| CoreError::public(ErrorCode::ModelOutputInvalid, "模型服务返回了无效响应"))?;
    let content = response
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content.into_text())
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| {
            CoreError::public(ErrorCode::ModelOutputInvalid, "模型没有返回结构化内容")
        })?;
    Ok((
        content,
        TokenUsage {
            prompt_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_tokens),
            completion_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens),
        },
    ))
}

fn extract_responses_output(body: Value) -> CoreResult<(String, TokenUsage)> {
    let response: ResponsesResponse = serde_json::from_value(body)
        .map_err(|_| CoreError::public(ErrorCode::ModelOutputInvalid, "模型服务返回了无效响应"))?;
    if let Some(status) = response.status.as_deref()
        && status != "completed"
    {
        return Err(CoreError::public(
            ErrorCode::ModelOutputInvalid,
            "模型响应未正常完成",
        ));
    }
    let content = response
        .output_text
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let text = response
                .output
                .into_iter()
                .filter(|item| item.kind == "message")
                .flat_map(|item| item.content)
                .filter(|part| part.kind == "output_text")
                .filter_map(|part| part.text)
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        })
        .ok_or_else(|| {
            CoreError::public(ErrorCode::ModelOutputInvalid, "模型没有返回结构化内容")
        })?;
    Ok((
        content,
        TokenUsage {
            prompt_tokens: response.usage.as_ref().and_then(|usage| usage.input_tokens),
            completion_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens),
        },
    ))
}

fn extract_anthropic_output(body: Value) -> CoreResult<(String, TokenUsage)> {
    let response: AnthropicResponse = serde_json::from_value(body)
        .map_err(|_| CoreError::public(ErrorCode::ModelOutputInvalid, "模型服务返回了无效响应"))?;
    if matches!(
        response.stop_reason.as_deref(),
        Some("max_tokens" | "refusal")
    ) {
        return Err(CoreError::public(
            ErrorCode::ModelOutputInvalid,
            "模型响应未完整结束",
        ));
    }
    let content = response
        .content
        .into_iter()
        .filter(|part| part.kind == "text")
        .filter_map(|part| part.text)
        .collect::<Vec<_>>()
        .join("\n");
    if content.trim().is_empty() {
        return Err(CoreError::public(
            ErrorCode::ModelOutputInvalid,
            "模型没有返回结构化内容",
        ));
    }
    Ok((
        content,
        TokenUsage {
            prompt_tokens: response.usage.as_ref().and_then(|usage| usage.input_tokens),
            completion_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens),
        },
    ))
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

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: ChatContent,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

impl ChatContent {
    fn into_text(self) -> String {
        match self {
            Self::Text(value) => value,
            Self::Parts(parts) => parts
                .into_iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text)
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Deserialize)]
struct ChatContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<i32>,
    #[serde(default)]
    completion_tokens: Option<i32>,
}

#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<ResponsesOutputContent>,
}

#[derive(Deserialize)]
struct ResponsesOutputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: Option<i32>,
    #[serde(default)]
    output_tokens: Option<i32>,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<i32>,
    #[serde(default)]
    output_tokens: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> ProviderProfile {
        ProviderProfile {
            base_url: "https://example.test/v1".to_owned(),
            api_key: SecretString::from("test-key"),
            candidate_model: "candidate".to_owned(),
            verifier_model: "verifier".to_owned(),
            candidate_system_prompt: "candidate system".to_owned(),
            verifier_system_prompt: "verifier system".to_owned(),
        }
    }

    #[test]
    fn accepts_only_supported_provider_kinds() {
        assert_eq!(
            ProviderKind::parse("openai_responses").unwrap(),
            ProviderKind::OpenAiResponses
        );
        assert_eq!(
            ProviderKind::parse("openai_compatible").unwrap(),
            ProviderKind::OpenAiCompatible
        );
        assert_eq!(
            ProviderKind::parse("anthropic").unwrap(),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::parse("custom").unwrap_err().code(),
            ErrorCode::ConfigurationInvalid
        );
    }

    #[test]
    fn protocol_endpoints_are_appended_once() {
        assert_eq!(
            endpoint_for("https://api.openai.com/v1", ProviderKind::OpenAiResponses),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            endpoint_for(
                "https://gateway.test/v1/chat/completions/",
                ProviderKind::OpenAiCompatible
            ),
            "https://gateway.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint_for("https://api.anthropic.com/v1", ProviderKind::Anthropic),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn builds_each_protocol_request_shape() {
        let compatible =
            JsonProtocolProvider::new(ProviderKind::OpenAiCompatible, sample_profile()).unwrap();
        let compatible_request = compatible.request_body("model", "system", "user");
        assert_eq!(
            compatible_request["response_format"]["type"],
            json!("json_object")
        );
        assert_eq!(compatible_request["messages"][0]["role"], json!("system"));

        let responses =
            JsonProtocolProvider::new(ProviderKind::OpenAiResponses, sample_profile()).unwrap();
        let responses_request = responses.request_body("model", "system", "user");
        assert_eq!(responses_request["store"], json!(false));
        assert_eq!(
            responses_request["text"]["format"]["type"],
            json!("json_object")
        );
        assert_eq!(
            responses_request["input"][0]["content"][0]["type"],
            json!("input_text")
        );

        let anthropic =
            JsonProtocolProvider::new(ProviderKind::Anthropic, sample_profile()).unwrap();
        let anthropic_request = anthropic.request_body("model", "system", "user");
        assert_eq!(
            anthropic_request["max_tokens"],
            json!(ANTHROPIC_MAX_OUTPUT_TOKENS)
        );
        assert_eq!(anthropic_request["system"], json!("system"));
        assert_eq!(anthropic_request["messages"][0]["role"], json!("user"));
    }

    #[test]
    fn extracts_native_responses_and_usage() {
        let (content, usage) = extract_provider_output(
            ProviderKind::OpenAiResponses,
            json!({
                "status": "completed",
                "output": [{
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "{\"ok\":true}" }]
                }],
                "usage": { "input_tokens": 12, "output_tokens": 5 }
            }),
        )
        .unwrap();
        assert_eq!(content, "{\"ok\":true}");
        assert_eq!(usage.prompt_tokens, Some(12));
        assert_eq!(usage.completion_tokens, Some(5));

        let (content, usage) = extract_provider_output(
            ProviderKind::Anthropic,
            json!({
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "{\"ok\":true}" }],
                "usage": { "input_tokens": 7, "output_tokens": 4 }
            }),
        )
        .unwrap();
        assert_eq!(content, "{\"ok\":true}");
        assert_eq!(usage.prompt_tokens, Some(7));
        assert_eq!(usage.completion_tokens, Some(4));
    }

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
