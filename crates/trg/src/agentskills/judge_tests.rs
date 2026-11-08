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
fn base64_matches_the_standard_test_vectors() {
    assert_eq!(base64_encode(b"Man"), "TWFu");
    assert_eq!(base64_encode(b"Ma"), "TWE=");
    assert_eq!(base64_encode(b"M"), "TQ==");
    assert_eq!(base64_encode(b""), "");
}

#[test]
fn an_attached_image_becomes_a_provider_shaped_content_part() {
    let request = JudgeRequest::new("be a judge", "look at this")
        .with_image(ImageAttachment::new(ImageMediaType::Png, &[0, 1, 2]));

    let openai = build_body(JudgeApi::OpenAiChatCompletions, &model(), &request);
    let openai_content = &openai["messages"][1]["content"];
    assert_eq!(openai_content[0]["type"], "text");
    assert_eq!(openai_content[0]["text"], "look at this");
    assert_eq!(openai_content[1]["type"], "image_url");
    assert_eq!(openai_content[1]["image_url"]["url"], "data:image/png;base64,AAEC");

    let anthropic = build_body(JudgeApi::AnthropicMessages, &model(), &request);
    let anthropic_content = &anthropic["messages"][0]["content"];
    assert_eq!(anthropic_content[0]["type"], "text");
    assert_eq!(anthropic_content[1]["type"], "image");
    assert_eq!(anthropic_content[1]["source"]["type"], "base64");
    assert_eq!(anthropic_content[1]["source"]["media_type"], "image/png");
    assert_eq!(anthropic_content[1]["source"]["data"], "AAEC");
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

#[test]
fn a_model_named_on_the_command_line_wins_over_the_environment() {
    let env = MapEnv::new(&[(MODEL_ENV, "from-the-environment")]);

    let model = JudgeModel::resolve_from(&env, Some("from-the-flag"));

    assert_eq!(model.map(|model| model.to_string()), Some("from-the-flag".to_string()));
}

#[test]
fn a_pass_that_names_no_model_takes_the_one_the_environment_names() {
    let env = MapEnv::new(&[(MODEL_ENV, "from-the-environment")]);

    let model = JudgeModel::resolve_from(&env, None);

    assert_eq!(
        model.map(|model| model.to_string()),
        Some("from-the-environment".to_string())
    );
}

#[test]
fn an_environment_that_names_no_model_leaves_the_pass_without_one() {
    let env = MapEnv::new(&[(MODEL_ENV, "   ")]);

    assert!(JudgeModel::resolve_from(&env, None).is_none());
    assert!(JudgeModel::resolve_from(&env, Some(" ")).is_none());
    assert!(JudgeModel::resolve_from(&MapEnv::new(&[]), None).is_none());
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
