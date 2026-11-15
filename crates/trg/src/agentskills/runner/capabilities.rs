//! What each harness can be told to do, declared once rather than scattered across every
//! call site that needs to know.
//!
//! Every cell in the table below was read off the harness's own `--help` on 2026-09-12,
//! except the codex `mcp servers` cell, read off `codex mcp add --help` on 2026-09-14
//! together with a verified round trip of the `config.toml` that subcommand writes. A
//! cell may only change when the harness's own help output or the config file shape it
//! documents changes; it must never be inferred from another harness or extrapolated from
//! behaviour that was not read off the harness directly.
//!
//! codex publishes no counterpart to claude-code's `--strict-mcp-config`: `-c` merges into
//! the config home rather than replacing it, so the operator's own MCP servers stay live
//! beside anything trg adds. That is why the codex `mcp servers` cell is only honoured
//! where trg owns the config home, which is what `--environment isolated` creates.
//!
//! claude-code's `--max-turns` is the one cell its `--help` does not list: the flag is
//! hidden there, and was read instead off a `claude -p` run on 2026-10-11 (version
//! 2.1.283) that accepted it where an unknown option is refused outright.

use super::Runner;
use ControlMechanism::{ConfigFile, EnvVar, Flag, GuardedFlag, Reported, Subcommand};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum HarnessControl {
    ToolAllowlist,
    TurnCap,
    SystemPromptAppend,
    McpServers,
    SandboxLevels,
    ConversationResume,
    ConversationSeeding,
    RunScopedConfigHome,
    CostReporting,
}

impl HarnessControl {
    pub const ALL: [HarnessControl; 9] = [
        Self::ToolAllowlist,
        Self::TurnCap,
        Self::SystemPromptAppend,
        Self::McpServers,
        Self::SandboxLevels,
        Self::ConversationResume,
        Self::ConversationSeeding,
        Self::RunScopedConfigHome,
        Self::CostReporting,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ToolAllowlist => "tool allowlist",
            Self::TurnCap => "turn cap",
            Self::SystemPromptAppend => "system prompt append",
            Self::McpServers => "mcp servers",
            Self::SandboxLevels => "sandbox levels",
            Self::ConversationResume => "conversation resume",
            Self::ConversationSeeding => "conversation seeding",
            Self::RunScopedConfigHome => "run-scoped config home",
            Self::CostReporting => "cost reporting",
        }
    }
}

/// The concrete shape of a control once a harness offers it: a flag, a subcommand, an
/// env var, or a fact the harness simply reports. Kept apart from `ControlSupport` so the
/// question "what does the harness expose" has a value even when trg does not drive it.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ControlMechanism {
    Flag(&'static str),
    Subcommand(&'static str),
    EnvVar(&'static str),
    Reported,
    /// A flag whose effect only holds while a second, guarding flag is also present.
    ///
    /// `--mcp-config` alone still lets claude-code fall back to whatever MCP servers are
    /// configured on the operator's machine; `--strict-mcp-config` is what makes the
    /// declared config the whole story. The two only mean anything together, so they are one
    /// mechanism, not two cells' worth of flags smuggled into one.
    GuardedFlag {
        value: &'static str,
        guard: &'static str,
    },
    /// A control delivered by writing a file into the harness's config home rather than by
    /// anything on the command line.
    ///
    /// It carries no exclusivity of its own: whatever the config home already held is
    /// still in force, so a run only gets what it declared when it owns that home.
    ConfigFile {
        home_var: &'static str,
        file: &'static str,
    },
}

impl ControlMechanism {
    fn describe(self) -> String {
        match self {
            Self::Flag(flag) => format!("`{flag}`"),
            Self::Subcommand(subcommand) => format!("`{subcommand}` subcommand"),
            Self::EnvVar(var) => format!("`{var}`"),
            Self::Reported => "reported".to_string(),
            Self::GuardedFlag { value, guard } => format!("`{value}` (guarded by `{guard}`)"),
            Self::ConfigFile { home_var, file } => format!("`{file}` in `${home_var}`"),
        }
    }
}

/// Whether a harness offers a control is a fact about its own CLI; whether trg drives that
/// control is a fact about trg's invocation code. A single `Flag`-like value used to answer
/// both questions at once, which let a cell claim a mechanism no runner ever read. Keeping
/// `Driven` apart from `Offered` means only a cell trg actually exercises can hand out a
/// mechanism string, so an argument builder can never be built from a cell trg does not
/// drive.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ControlSupport {
    /// The harness does not offer this control.
    Absent,
    /// The harness offers it; trg does not exercise it.
    Offered(ControlMechanism),
    /// The harness offers it and trg builds its invocation from this cell.
    Driven(ControlMechanism),
}

impl ControlSupport {
    pub fn is_offered(self) -> bool {
        !matches!(self, Self::Absent)
    }

    pub fn flag(self) -> Option<&'static str> {
        match self {
            Self::Driven(ControlMechanism::Flag(flag)) => Some(flag),
            Self::Driven(ControlMechanism::GuardedFlag { value, .. }) => Some(value),
            _ => None,
        }
    }

    /// The flag that must accompany `flag()` for a `GuardedFlag` cell to mean what it says.
    pub fn guard_flag(self) -> Option<&'static str> {
        match self {
            Self::Driven(ControlMechanism::GuardedFlag { guard, .. }) => Some(guard),
            _ => None,
        }
    }

    /// The file in the harness's config home a `ConfigFile` cell is delivered through, so
    /// a runner writes the name this table declares rather than one of its own.
    pub fn config_file_name(self) -> Option<&'static str> {
        match self {
            Self::Driven(ControlMechanism::ConfigFile { file, .. }) => Some(file),
            _ => None,
        }
    }

    /// Whether this control is only honest once the run owns the harness's config home.
    ///
    /// A config file carries no exclusivity: the operator's own entries in that home stay
    /// in force alongside the ones trg wrote, so a run under a config home trg does not
    /// own is not the run the report would describe.
    pub fn needs_run_owned_config_home(self) -> bool {
        matches!(self, Self::Driven(ControlMechanism::ConfigFile { .. }))
    }

    pub fn describe(self) -> String {
        match self {
            Self::Absent => "no".to_string(),
            Self::Offered(mechanism) => format!("{} (harness only)", mechanism.describe()),
            Self::Driven(mechanism) => mechanism.describe(),
        }
    }
}

impl Runner {
    pub fn support(self, control: HarnessControl) -> ControlSupport {
        match (self, control) {
            (Self::ClaudeCode, HarnessControl::ToolAllowlist) => ControlSupport::Driven(Flag("--allowedTools")),
            (Self::ClaudeCode, HarnessControl::TurnCap) => ControlSupport::Driven(Flag("--max-turns")),
            (Self::ClaudeCode, HarnessControl::SystemPromptAppend) => {
                ControlSupport::Driven(Flag("--append-system-prompt"))
            }
            (Self::ClaudeCode, HarnessControl::McpServers) => ControlSupport::Driven(GuardedFlag {
                value: "--mcp-config",
                guard: "--strict-mcp-config",
            }),
            (Self::ClaudeCode, HarnessControl::SandboxLevels) => ControlSupport::Driven(Flag("--permission-mode")),
            (Self::ClaudeCode, HarnessControl::ConversationResume) => ControlSupport::Offered(Flag("--resume")),
            // `--input-format stream-json` looked like a candidate, but every "user" event
            // it is given re-runs as a live model turn and any scripted "assistant" event
            // is discarded, so it cannot adopt a case-authored transcript as history that
            // already happened. Nothing in claude's `--help` output does that.
            (Self::ClaudeCode, HarnessControl::ConversationSeeding) => ControlSupport::Absent,
            (Self::ClaudeCode, HarnessControl::RunScopedConfigHome) => {
                ControlSupport::Driven(EnvVar("CLAUDE_CONFIG_DIR"))
            }
            (Self::ClaudeCode, HarnessControl::CostReporting) => ControlSupport::Driven(Reported),

            (Self::Codex, HarnessControl::ToolAllowlist) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::SystemPromptAppend) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::McpServers) => ControlSupport::Driven(ConfigFile {
                home_var: "CODEX_HOME",
                file: "config.toml",
            }),
            (Self::Codex, HarnessControl::SandboxLevels) => ControlSupport::Driven(Flag("-s")),
            (Self::Codex, HarnessControl::ConversationResume) => ControlSupport::Offered(Subcommand("resume")),
            // `codex exec resume` only replays a session codex itself recorded; there is no
            // flag or subcommand that ingests an arbitrary transcript.
            (Self::Codex, HarnessControl::ConversationSeeding) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::RunScopedConfigHome) => ControlSupport::Driven(EnvVar("CODEX_HOME")),
            (Self::Codex, HarnessControl::CostReporting) => ControlSupport::Absent,

            (Self::CursorAgent, HarnessControl::ToolAllowlist) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::SystemPromptAppend) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::McpServers) => ControlSupport::Absent,
            // cursor-agent accepts a sandbox level through --force, but it is the CLI's
            // only documented non-interactive grant: both permission grants collapse onto
            // this one flag rather than onto two distinct levels.
            (Self::CursorAgent, HarnessControl::SandboxLevels) => ControlSupport::Driven(Flag("--force")),
            (Self::CursorAgent, HarnessControl::ConversationResume) => ControlSupport::Offered(Flag("--resume")),
            // `--resume`/`--continue` only reopen a chat cursor-agent itself recorded; there
            // is no flag that ingests an arbitrary transcript.
            (Self::CursorAgent, HarnessControl::ConversationSeeding) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::RunScopedConfigHome) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::CostReporting) => ControlSupport::Absent,
        }
    }

    pub fn unsupported_reason(self, control: HarnessControl) -> String {
        format!("the {} harness offers no {}", self.display_name(), control.label())
    }
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
