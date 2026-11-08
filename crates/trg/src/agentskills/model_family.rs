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
#[path = "model_family_tests.rs"]
mod tests;
