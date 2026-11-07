//! The judge trg talks to when a grader or a comparison needs a model's opinion.
//!
//! The runner under evaluation and the judge doing the evaluating are separate
//! choices: grading a `codex` run with an Anthropic judge, or a `claude-code`
//! run with a local OpenAI-compatible endpoint, are both ordinary. This module
//! is therefore keyed on the wire protocol the judge endpoint speaks, not on
//! the harness that produced the transcript.

mod constants;
pub use constants::{ANTHROPIC_API_VERSION, API_KEY_ENV, BASE_URL_ENV, MODEL_ENV};
use schemars::JsonSchema;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::evals::EvalError;
use super::graders::ImageMediaType;
use super::validation::ValidationError;
use crate::telemetry::ContentCapture;

mod telemetry;

/// The request protocol a judge endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeApi {
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize, Deserialize, JsonSchema)]
pub enum JudgeProvider {
    #[default]
    #[value(name = "openai")]
    #[serde(rename = "openai")]
    OpenAi,
    #[value(name = "anthropic")]
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Any endpoint speaking the OpenAI chat-completions protocol, addressed
    /// through `TRG_JUDGE_BASE_URL` and `TRG_JUDGE_API_KEY`.
    #[value(name = "compatible")]
    #[serde(rename = "compatible")]
    Compatible,
}

impl JudgeProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Compatible => "compatible",
        }
    }

    pub fn api(self) -> JudgeApi {
        match self {
            Self::OpenAi | Self::Compatible => JudgeApi::OpenAiChatCompletions,
            Self::Anthropic => JudgeApi::AnthropicMessages,
        }
    }

    fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Anthropic => Some("https://api.anthropic.com/v1"),
            Self::Compatible => None,
        }
    }

    fn default_api_key_env(self) -> &'static str {
        match self {
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Compatible => API_KEY_ENV,
        }
    }
}

/// A judge model identifier that is known to be non-blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeModel(String);

impl JudgeModel {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return None;
        }
        Some(Self(value))
    }

    /// The model to judge with: the flag when one was passed, and the environment
    /// otherwise.
    ///
    /// The endpoint and the credential already come from the environment when no flag
    /// names them, and the model is the remaining reason a judged pass needs a second
    /// flag on every invocation. It is read here rather than defaulted to a built-in
    /// identifier because a model name compiled into a release outlives the model.
    pub fn resolve_from(env: &impl JudgeEnv, flag: Option<&str>) -> Option<Self> {
        flag.and_then(Self::new)
            .or_else(|| env.non_empty(MODEL_ENV).and_then(Self::new))
    }

    pub fn resolve(flag: Option<&str>) -> Option<Self> {
        Self::resolve_from(&ProcessEnv, flag)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JudgeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where the environment variables a judge is addressed through are read from.
///
/// A trait rather than a direct `std::env::var` call so the resolution rules
/// can be tested without mutating the process environment, which tests running
/// in parallel share.
pub trait JudgeEnv {
    fn var(&self, key: &str) -> Option<String>;

    fn non_empty(&self, key: &str) -> Option<String> {
        self.var(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
}

pub struct ProcessEnv;

impl JudgeEnv for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// A resolved endpoint and credential, ready to be called.
#[derive(Clone)]
pub struct JudgeEndpoint {
    pub provider: JudgeProvider,
    pub model: JudgeModel,
    pub base_url: String,
    api_key: SecretString,
}

impl std::fmt::Debug for JudgeEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JudgeEndpoint")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl JudgeEndpoint {
    /// Resolves the endpoint from the provider defaults and the process
    /// environment.
    ///
    /// `field` names the flag the caller is validating, so `compare
    /// --judge-model` and `grade --grader-model` each report against their own
    /// flag.
    pub fn resolve(provider: JudgeProvider, model: &JudgeModel, field: &str) -> Result<Self, EvalError> {
        Self::resolve_from(&ProcessEnv, provider, model, field)
    }

    pub fn resolve_from(
        env: &impl JudgeEnv,
        provider: JudgeProvider,
        model: &JudgeModel,
        field: &str,
    ) -> Result<Self, EvalError> {
        let base_url = match env.non_empty(BASE_URL_ENV) {
            Some(value) => value.trim_end_matches('/').to_string(),
            None => provider
                .default_base_url()
                .ok_or_else(|| {
                    invalid(
                        field,
                        format!(
                            "provider '{}' has no built-in endpoint, so {BASE_URL_ENV} must be set",
                            provider.as_str()
                        ),
                    )
                })?
                .to_string(),
        };

        let key_env = provider.default_api_key_env();
        let api_key = env
            .non_empty(API_KEY_ENV)
            .or_else(|| env.non_empty(key_env))
            .ok_or_else(|| {
                invalid(
                    field,
                    format!(
                        "{key_env} (or {API_KEY_ENV}) must be set to judge with provider '{}'",
                        provider.as_str()
                    ),
                )
            })?;

        Ok(Self {
            provider,
            model: model.clone(),
            base_url,
            api_key: SecretString::from(api_key),
        })
    }

    pub fn url(&self) -> String {
        match self.provider.api() {
            JudgeApi::OpenAiChatCompletions => format!("{}/chat/completions", self.base_url),
            JudgeApi::AnthropicMessages => format!("{}/messages", self.base_url),
        }
    }
}

/// A picture attached to a judge request, already carrying the encoding both
/// providers' vision content parts require.
///
/// Built from `graders::ImageMediaType` plus the file's bytes rather than a
/// path, so a request can be assembled (and, in tests, inspected) without a
/// filesystem in the loop.
#[derive(Debug, Clone)]
pub struct ImageAttachment {
    media_type: ImageMediaType,
    base64_data: String,
}

impl ImageAttachment {
    pub fn new(media_type: ImageMediaType, bytes: &[u8]) -> Self {
        Self {
            media_type,
            base64_data: base64_encode(bytes),
        }
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// One judging turn: a system instruction, the payload to judge, and
/// optionally a picture the payload refers to.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    pub system: String,
    pub user: String,
    pub image: Option<ImageAttachment>,
    pub max_tokens: u32,
}

impl JudgeRequest {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
            image: None,
            max_tokens: 1024,
        }
    }

    pub fn with_image(mut self, image: ImageAttachment) -> Self {
        self.image = Some(image);
        self
    }
}

/// The `content` value of the user turn: a bare string when there is nothing
/// to look at beyond the text, or a provider-shaped array of content parts
/// once a picture is attached, since neither provider accepts an image
/// alongside a bare string.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UserPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ImageUrl },
    Image { source: ImageSource<'a> },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Serialize)]
struct ImageSource<'a> {
    #[serde(rename = "type")]
    encoding: &'static str,
    media_type: &'static str,
    data: &'a str,
}

#[derive(Serialize)]
struct JudgeMessage {
    role: &'static str,
    content: serde_json::Value,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [JudgeMessage; 2],
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: [JudgeMessage; 1],
}

fn user_content(api: JudgeApi, request: &JudgeRequest) -> serde_json::Value {
    let Some(image) = &request.image else {
        return serde_json::Value::String(request.user.clone());
    };
    let image_part = match api {
        JudgeApi::OpenAiChatCompletions => UserPart::ImageUrl {
            image_url: ImageUrl {
                url: format!("data:{};base64,{}", image.media_type.mime(), image.base64_data),
            },
        },
        JudgeApi::AnthropicMessages => UserPart::Image {
            source: ImageSource {
                encoding: "base64",
                media_type: image.media_type.mime(),
                data: &image.base64_data,
            },
        },
    };
    serde_json::to_value([UserPart::Text { text: &request.user }, image_part])
        .expect("judge content contains only JSON values")
}

pub fn build_body(api: JudgeApi, model: &JudgeModel, request: &JudgeRequest) -> serde_json::Value {
    let user_message = JudgeMessage {
        role: "user",
        content: user_content(api, request),
    };
    match api {
        JudgeApi::OpenAiChatCompletions => serde_json::to_value(ChatRequest {
            model: model.as_str(),
            messages: [
                JudgeMessage {
                    role: "system",
                    content: serde_json::Value::String(request.system.clone()),
                },
                user_message,
            ],
            response_format: ResponseFormat { kind: "json_object" },
        }),
        JudgeApi::AnthropicMessages => serde_json::to_value(MessagesRequest {
            model: model.as_str(),
            max_tokens: request.max_tokens,
            system: &request.system,
            messages: [user_message],
        }),
    }
    .expect("judge requests contain only JSON values")
}

pub fn extract_content(api: JudgeApi, payload: &serde_json::Value) -> Option<String> {
    let pointer = match api {
        JudgeApi::OpenAiChatCompletions => "/choices/0/message/content",
        JudgeApi::AnthropicMessages => "/content/0/text",
    };
    payload
        .pointer(pointer)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Token counts a judge endpoint reported for one turn, normalized to the
/// GenAI conventions: `input_tokens` includes the cached tokens, which the
/// cache fields then break out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JudgeUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
}

impl JudgeUsage {
    pub fn from_payload(api: JudgeApi, payload: &serde_json::Value) -> Self {
        let count = |pointer: &str| payload.pointer(pointer).and_then(serde_json::Value::as_u64);
        match api {
            JudgeApi::OpenAiChatCompletions => Self {
                input_tokens: count("/usage/prompt_tokens"),
                output_tokens: count("/usage/completion_tokens"),
                cache_read_input_tokens: count("/usage/prompt_tokens_details/cached_tokens"),
                cache_write_input_tokens: None,
            },
            JudgeApi::AnthropicMessages => {
                let cache_read = count("/usage/cache_read_input_tokens");
                let cache_write = count("/usage/cache_creation_input_tokens");
                Self {
                    input_tokens: count("/usage/input_tokens")
                        .map(|uncached| uncached + cache_read.unwrap_or(0) + cache_write.unwrap_or(0)),
                    output_tokens: count("/usage/output_tokens"),
                    cache_read_input_tokens: cache_read,
                    cache_write_input_tokens: cache_write,
                }
            }
        }
    }
}

/// What a judge endpoint answered one turn with: the text callers parse their
/// verdict from, and what the endpoint said about the call itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeReply {
    pub content: String,
    pub response_model: Option<JudgeModel>,
    pub response_id: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: JudgeUsage,
}

impl JudgeReply {
    pub fn from_payload(api: JudgeApi, payload: &serde_json::Value) -> Option<Self> {
        let text = |pointer: &str| {
            payload
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let finish_reason = match api {
            JudgeApi::OpenAiChatCompletions => text("/choices/0/finish_reason"),
            JudgeApi::AnthropicMessages => text("/stop_reason"),
        };
        Some(Self {
            content: extract_content(api, payload)?,
            response_model: text("/model").and_then(JudgeModel::new),
            response_id: text("/id"),
            finish_reason,
            usage: JudgeUsage::from_payload(api, payload),
        })
    }
}

/// Sends one judging turn and returns the model's reply.
///
/// Callers parse their own verdict shape out of the reply: a comparison wants a
/// winner, a grader wants a pass or fail.
///
/// A client is built per call. Ballots run one after another and each waits on
/// a model for seconds, so the connection pool a shared client would keep buys
/// nothing measurable here.
pub fn judge(endpoint: &JudgeEndpoint, request: &JudgeRequest, field: &str) -> Result<JudgeReply, EvalError> {
    let inference = telemetry::InferenceSpan::start(endpoint, request, ContentCapture::from_env());
    let result = inference.span().in_scope(|| exchange(endpoint, request, &inference));
    inference.finish(&result);
    result.map_err(|failure| invalid(field, failure.message))
}

fn exchange(
    endpoint: &JudgeEndpoint,
    request: &JudgeRequest,
    inference: &telemetry::InferenceSpan,
) -> Result<JudgeReply, JudgeFailure> {
    let api = endpoint.provider.api();
    let body = build_body(api, &endpoint.model, request);

    let client = reqwest::blocking::Client::new();
    let builder = match api {
        JudgeApi::OpenAiChatCompletions => client
            .post(endpoint.url())
            .bearer_auth(endpoint.api_key.expose_secret()),
        JudgeApi::AnthropicMessages => client
            .post(endpoint.url())
            .header("x-api-key", endpoint.api_key.expose_secret())
            .header("anthropic-version", ANTHROPIC_API_VERSION),
    };

    let http = inference.http();
    let payload = http.span().in_scope(|| {
        let response = builder
            .json(&body)
            .send()
            .map_err(|source| JudgeFailure::transport(&source))?;
        let status = response.status();
        http.responded(status);
        if !status.is_success() {
            let detail = response.text().unwrap_or_default();
            return Err(JudgeFailure::new(
                JudgeFailureKind::Status(status),
                format!("judge request to {} failed with {status}: {detail}", endpoint.url()),
            ));
        }
        response
            .json::<serde_json::Value>()
            .map_err(|source| JudgeFailure::new(JudgeFailureKind::InvalidResponse, source.to_string()))
    });
    http.finish(&payload);

    JudgeReply::from_payload(api, &payload?).ok_or_else(|| {
        JudgeFailure::new(
            JudgeFailureKind::NoContent,
            format!(
                "judge provider '{}' returned no message content",
                endpoint.provider.as_str()
            ),
        )
    })
}

/// Why one judging turn produced no reply, kept apart from its message so the
/// span records a low-cardinality `error.type` while the caller sees the detail.
#[derive(Debug)]
struct JudgeFailure {
    kind: JudgeFailureKind,
    message: String,
}

#[derive(Debug, Clone, Copy)]
enum JudgeFailureKind {
    Timeout,
    Connect,
    Transport,
    Status(reqwest::StatusCode),
    InvalidResponse,
    NoContent,
}

impl JudgeFailure {
    fn new(kind: JudgeFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn transport(source: &reqwest::Error) -> Self {
        let kind = if source.is_timeout() {
            JudgeFailureKind::Timeout
        } else if source.is_connect() {
            JudgeFailureKind::Connect
        } else {
            JudgeFailureKind::Transport
        };
        Self::new(kind, source.to_string())
    }

    fn error_type(&self) -> std::borrow::Cow<'static, str> {
        match self.kind {
            JudgeFailureKind::Timeout => "timeout".into(),
            JudgeFailureKind::Connect => "connect".into(),
            JudgeFailureKind::Transport => "transport".into(),
            JudgeFailureKind::Status(status) => status.as_u16().to_string().into(),
            JudgeFailureKind::InvalidResponse => "invalid_response".into(),
            JudgeFailureKind::NoContent => "no_content".into(),
        }
    }
}

/// Parses a judge reply that is JSON, tolerating the fenced code block models
/// often wrap it in.
pub fn parse_json_reply<T: for<'de> Deserialize<'de>>(reply: &str, field: &str) -> Result<T, EvalError> {
    let trimmed = strip_code_fence(reply.trim());
    serde_json::from_str(trimmed).map_err(|source| invalid(field, format!("judge returned invalid JSON: {source}")))
}

fn strip_code_fence(reply: &str) -> &str {
    let Some(rest) = reply.strip_prefix("```") else {
        return reply;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start_matches('\n')
        .trim_end()
        .trim_end_matches("```")
        .trim_end()
}

fn invalid(field: &str, message: impl Into<String>) -> EvalError {
    EvalError::Validation(ValidationError::for_field(field, message).into())
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
