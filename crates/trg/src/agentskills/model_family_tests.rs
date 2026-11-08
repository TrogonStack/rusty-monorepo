use super::*;

#[test]
fn a_model_identifier_names_its_family_through_gateway_prefixes_and_aliases() {
    for (model, family) in [
        ("claude-sonnet-4-5", ModelFamily::Claude),
        ("us.anthropic.claude-3-7-sonnet-20250219-v1:0", ModelFamily::Claude),
        ("openrouter/anthropic/claude-opus-4.1", ModelFamily::Claude),
        ("sonnet", ModelFamily::Claude),
        ("opus-4.1", ModelFamily::Claude),
        ("gpt-5-codex", ModelFamily::Gpt),
        ("o3-mini", ModelFamily::Gpt),
        ("openai/gpt-4o", ModelFamily::Gpt),
        ("gemini-2.5-pro", ModelFamily::Gemini),
        ("meta-llama/Llama-3.3-70B-Instruct", ModelFamily::Llama),
        ("mixtral-8x7b", ModelFamily::Mistral),
        ("grok-4", ModelFamily::Grok),
        ("deepseek-r1", ModelFamily::Deepseek),
        ("Qwen/Qwen3-Coder", ModelFamily::Qwen),
    ] {
        assert_eq!(ModelFamily::of(model), Some(family), "{model}");
    }
}

#[test]
fn an_identifier_naming_no_known_family_has_none() {
    for model in ["auto", "my-finetune", "o5-preview-but-not-really"] {
        assert_eq!(ModelFamily::of(model), None, "{model}");
    }
}
