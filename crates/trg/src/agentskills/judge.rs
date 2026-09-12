//! The judge trg talks to when a grader or a comparison needs a model's opinion.
//!
//! The runner under evaluation and the judge doing the evaluating are separate
//! choices: grading a `codex` run with an Anthropic judge, or a `claude-code`
//! run with a local OpenAI-compatible endpoint, are both ordinary. This module
//! is therefore keyed on the wire protocol the judge endpoint speaks, not on
//! the harness that produced the transcript.

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use super::evals::EvalError;
use super::validation::ValidationError;

/// The request protocol a judge endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeApi {
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum JudgeProvider {
    #[default]
    #[value(name = "openai")]
    OpenAi,
    #[value(name = "anthropic")]
    Anthropic,
    /// Any endpoint speaking the OpenAI chat-completions protocol, addressed
    /// through `TRG_JUDGE_BASE_URL` and `TRG_JUDGE_API_KEY`.
    #[value(name = "compatible")]
    Compatible,
}

pub const BASE_URL_ENV: &str = "TRG_JUDGE_BASE_URL";
pub const API_KEY_ENV: &str = "TRG_JUDGE_API_KEY";
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";

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

/// One judging turn: a system instruction and the payload to judge.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
}

impl JudgeRequest {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
            max_tokens: 1024,
        }
    }
}

pub fn build_body(api: JudgeApi, model: &JudgeModel, request: &JudgeRequest) -> serde_json::Value {
    match api {
        JudgeApi::OpenAiChatCompletions => serde_json::json!({
            "model": model.as_str(),
            "messages": [
                {"role": "system", "content": request.system},
                {"role": "user", "content": request.user}
            ],
            "response_format": {"type": "json_object"}
        }),
        JudgeApi::AnthropicMessages => serde_json::json!({
            "model": model.as_str(),
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": [{"role": "user", "content": request.user}]
        }),
    }
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

/// Sends one judging turn and returns the model's raw text reply.
///
/// Callers parse their own verdict shape out of the reply: a comparison wants a
/// winner, a grader wants a pass or fail.
pub fn judge(endpoint: &JudgeEndpoint, request: &JudgeRequest, field: &str) -> Result<String, EvalError> {
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

    let response = builder
        .json(&body)
        .send()
        .map_err(|source| invalid(field, source.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(invalid(
            field,
            format!("judge request to {} failed with {status}: {detail}", endpoint.url()),
        ));
    }

    let payload: serde_json::Value = response.json().map_err(|source| invalid(field, source.to_string()))?;
    extract_content(api, &payload).ok_or_else(|| {
        invalid(
            field,
            format!(
                "judge provider '{}' returned no message content",
                endpoint.provider.as_str()
            ),
        )
    })
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
mod tests {
    use super::*;

    fn model() -> JudgeModel {
        JudgeModel::new("judge-model").unwrap()
    }

    #[test]
    fn blank_models_are_rejected() {
        assert!(JudgeModel::new("   ").is_none());
        assert_eq!(JudgeModel::new(" m ").unwrap().as_str(), " m ");
    }

    #[test]
    fn openai_and_anthropic_bodies_use_their_own_vocabulary() {
        let request = JudgeRequest::new("be a judge", "{\"a\":1}");

        let openai = build_body(JudgeApi::OpenAiChatCompletions, &model(), &request);
        assert_eq!(openai["messages"][0]["role"], "system");
        assert_eq!(openai["messages"][1]["content"], "{\"a\":1}");
        assert_eq!(openai["response_format"]["type"], "json_object");
        assert!(openai.get("max_tokens").is_none());

        let anthropic = build_body(JudgeApi::AnthropicMessages, &model(), &request);
        assert_eq!(anthropic["system"], "be a judge");
        assert_eq!(anthropic["messages"][0]["role"], "user");
        assert_eq!(anthropic["max_tokens"], 1024);
        assert!(anthropic.get("response_format").is_none());
    }

    #[test]
    fn content_is_read_from_each_providers_own_response_shape() {
        let openai = serde_json::json!({"choices": [{"message": {"content": "verdict"}}]});
        assert_eq!(
            extract_content(JudgeApi::OpenAiChatCompletions, &openai).as_deref(),
            Some("verdict")
        );

        let anthropic = serde_json::json!({"content": [{"type": "text", "text": "verdict"}]});
        assert_eq!(
            extract_content(JudgeApi::AnthropicMessages, &anthropic).as_deref(),
            Some("verdict")
        );

        assert!(extract_content(JudgeApi::AnthropicMessages, &openai).is_none());
    }

    struct MapEnv(std::collections::HashMap<&'static str, &'static str>);

    impl MapEnv {
        fn new(entries: &[(&'static str, &'static str)]) -> Self {
            Self(entries.iter().copied().collect())
        }
    }

    impl JudgeEnv for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|value| (*value).to_string())
        }
    }

    fn endpoint(provider: JudgeProvider, env: &MapEnv) -> Result<JudgeEndpoint, EvalError> {
        JudgeEndpoint::resolve_from(env, provider, &model(), "judge_model")
    }

    #[test]
    fn urls_follow_the_protocol_not_the_provider_name() {
        let env = MapEnv::new(&[(BASE_URL_ENV, "https://example.test/v1"), (API_KEY_ENV, "k")]);
        for (provider, expected) in [
            (JudgeProvider::OpenAi, "https://example.test/v1/chat/completions"),
            (JudgeProvider::Anthropic, "https://example.test/v1/messages"),
            (JudgeProvider::Compatible, "https://example.test/v1/chat/completions"),
        ] {
            assert_eq!(
                endpoint(provider, &env).unwrap().url(),
                expected,
                "{}",
                provider.as_str()
            );
        }
    }

    #[test]
    fn fenced_json_replies_are_parsed() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Verdict {
            winner: String,
        }

        let fenced = "```json\n{\"winner\":\"A\"}\n```";
        let parsed: Verdict = parse_json_reply(fenced, "judge_model").unwrap();
        assert_eq!(parsed.winner, "A");

        let bare: Verdict = parse_json_reply("  {\"winner\":\"B\"}  ", "judge_model").unwrap();
        assert_eq!(bare.winner, "B");

        let err = parse_json_reply::<Verdict>("not json", "judge_model").unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
    }

    #[test]
    fn a_compatible_provider_without_a_base_url_names_the_variable_to_set() {
        let env = MapEnv::new(&[(API_KEY_ENV, "k")]);
        let err = endpoint(JudgeProvider::Compatible, &env).unwrap_err();
        assert!(err.to_string().contains(BASE_URL_ENV), "{err}");
    }

    #[test]
    fn a_missing_credential_names_the_providers_own_variable() {
        let env = MapEnv::new(&[]);
        let err = endpoint(JudgeProvider::Anthropic, &env).unwrap_err();
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"), "{err}");
        assert!(err.to_string().contains(API_KEY_ENV), "{err}");
    }

    #[test]
    fn the_shared_overrides_win_over_the_provider_defaults() {
        let env = MapEnv::new(&[
            (BASE_URL_ENV, "https://gateway.test/v1/"),
            (API_KEY_ENV, "shared-key"),
            ("OPENAI_API_KEY", "provider-key"),
        ]);
        let resolved = endpoint(JudgeProvider::OpenAi, &env).unwrap();
        assert_eq!(resolved.base_url, "https://gateway.test/v1");
        assert_eq!(resolved.api_key.expose_secret(), "shared-key");
    }

    #[test]
    fn a_provider_default_endpoint_is_used_when_no_override_is_set() {
        let env = MapEnv::new(&[("ANTHROPIC_API_KEY", "provider-key")]);
        let resolved = endpoint(JudgeProvider::Anthropic, &env).unwrap();
        assert_eq!(resolved.base_url, "https://api.anthropic.com/v1");
        assert_eq!(resolved.api_key.expose_secret(), "provider-key");
    }

    #[test]
    fn the_credential_never_appears_in_debug_output() {
        let env = MapEnv::new(&[(API_KEY_ENV, "super-secret")]);
        let resolved = endpoint(JudgeProvider::OpenAi, &env).unwrap();
        assert!(!format!("{resolved:?}").contains("super-secret"));
    }
}
