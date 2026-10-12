//! Which vendor's line of models a model identifier belongs to.
//!
//! A judge grading output written by a model of its own family tends to favour it, so
//! grading needs to notice when the judge and the runner share one. Recognised from the
//! identifier alone, since neither a harness nor a judge endpoint reports a family back.

use schemars::JsonSchema;
use serde::Serialize;

use super::runner::Runner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamily {
    Claude,
    Gpt,
    Gemini,
    Llama,
    Mistral,
    Grok,
    Deepseek,
    Qwen,
}

impl ModelFamily {
    /// The family a model identifier names, or `None` when it names none this recognises.
    ///
    /// Matched on the last path segment so a gateway prefix (`openrouter/anthropic/...`)
    /// does not hide the model, and on the bare `opus`, `sonnet`, and `haiku` aliases a
    /// harness accepts in place of a full Claude identifier.
    pub fn of(model: &str) -> Option<Self> {
        let lowered = model.to_ascii_lowercase();
        let name = lowered.rsplit('/').next().unwrap_or(&lowered);
        let first_token = name.split(['-', ':', '.', '@']).next().unwrap_or(name);

        if name.contains("claude") || matches!(first_token, "opus" | "sonnet" | "haiku") {
            return Some(Self::Claude);
        }
        if name.contains("gemini") {
            return Some(Self::Gemini);
        }
        if name.contains("gpt") || name.contains("codex") || matches!(first_token, "o1" | "o3" | "o4") {
            return Some(Self::Gpt);
        }
        [
            ("llama", Self::Llama),
            ("mistral", Self::Mistral),
            ("mixtral", Self::Mistral),
            ("codestral", Self::Mistral),
            ("devstral", Self::Mistral),
            ("grok", Self::Grok),
            ("deepseek", Self::Deepseek),
            ("qwen", Self::Qwen),
        ]
        .into_iter()
        .find_map(|(marker, family)| name.contains(marker).then_some(family))
    }

    /// The family a harness runs when it is given no model, for the harnesses that only
    /// ever default to one vendor's models.
    pub fn of_harness_default(runner: Runner) -> Option<Self> {
        match runner {
            Runner::ClaudeCode => Some(Self::Claude),
            Runner::Codex => Some(Self::Gpt),
            Runner::CursorAgent => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gpt => "gpt",
            Self::Gemini => "gemini",
            Self::Llama => "llama",
            Self::Mistral => "mistral",
            Self::Grok => "grok",
            Self::Deepseek => "deepseek",
            Self::Qwen => "qwen",
        }
    }
}

impl std::fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
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
}
