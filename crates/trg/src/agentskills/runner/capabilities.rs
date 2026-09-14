//! What each harness can be told to do, declared once rather than scattered across every
//! call site that needs to know.
//!
//! Every cell in the table below was read off the harness's own `--help` on 2026-09-12.
//! A cell may only change when the harness's help output changes; it must never be
//! inferred from another harness or extrapolated from behaviour that was not read off
//! `--help` directly.

use super::Runner;

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
}

impl ControlMechanism {
    fn describe(self) -> String {
        match self {
            Self::Flag(flag) => format!("`{flag}`"),
            Self::Subcommand(subcommand) => format!("`{subcommand}` subcommand"),
            Self::EnvVar(var) => format!("`{var}`"),
            Self::Reported => "reported".to_string(),
            Self::GuardedFlag { value, guard } => format!("`{value}` (guarded by `{guard}`)"),
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
        use ControlMechanism::{EnvVar, Flag, GuardedFlag, Reported, Subcommand};

        match (self, control) {
            (Self::ClaudeCode, HarnessControl::ToolAllowlist) => ControlSupport::Offered(Flag("--allowedTools")),
            (Self::ClaudeCode, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::ClaudeCode, HarnessControl::SystemPromptAppend) => {
                ControlSupport::Offered(Flag("--append-system-prompt"))
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
            (Self::Codex, HarnessControl::McpServers) => ControlSupport::Absent,
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
mod tests {
    use super::*;

    #[test]
    fn every_runner_answers_every_control() {
        let runners = [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent];
        let mut count = 0;
        for runner in runners {
            for control in HarnessControl::ALL {
                let _ = runner.support(control);
                count += 1;
            }
        }
        assert_eq!(count, 27);

        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::ToolAllowlist),
            ControlSupport::Offered(ControlMechanism::Flag("--allowedTools"))
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::TurnCap),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::SystemPromptAppend),
            ControlSupport::Offered(ControlMechanism::Flag("--append-system-prompt"))
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::McpServers),
            ControlSupport::Driven(ControlMechanism::GuardedFlag {
                value: "--mcp-config",
                guard: "--strict-mcp-config",
            })
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::SandboxLevels),
            ControlSupport::Driven(ControlMechanism::Flag("--permission-mode"))
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::ConversationResume),
            ControlSupport::Offered(ControlMechanism::Flag("--resume"))
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::ConversationSeeding),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::RunScopedConfigHome),
            ControlSupport::Driven(ControlMechanism::EnvVar("CLAUDE_CONFIG_DIR"))
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::CostReporting),
            ControlSupport::Driven(ControlMechanism::Reported)
        );

        assert_eq!(
            Runner::Codex.support(HarnessControl::ToolAllowlist),
            ControlSupport::Absent
        );
        assert_eq!(Runner::Codex.support(HarnessControl::TurnCap), ControlSupport::Absent);
        assert_eq!(
            Runner::Codex.support(HarnessControl::SystemPromptAppend),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::McpServers),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::SandboxLevels),
            ControlSupport::Driven(ControlMechanism::Flag("-s"))
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::ConversationResume),
            ControlSupport::Offered(ControlMechanism::Subcommand("resume"))
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::ConversationSeeding),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::RunScopedConfigHome),
            ControlSupport::Driven(ControlMechanism::EnvVar("CODEX_HOME"))
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::CostReporting),
            ControlSupport::Absent
        );

        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::ToolAllowlist),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::TurnCap),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::SystemPromptAppend),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::McpServers),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::SandboxLevels),
            ControlSupport::Driven(ControlMechanism::Flag("--force"))
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::ConversationResume),
            ControlSupport::Offered(ControlMechanism::Flag("--resume"))
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::ConversationSeeding),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::RunScopedConfigHome),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::CostReporting),
            ControlSupport::Absent
        );
    }

    #[test]
    fn every_driven_flag_is_carried_by_the_invocation_it_drives() {
        use crate::agentskills::report::PermissionGrant;
        use std::ffi::OsString;
        use std::path::Path;

        let argv_for = |runner: Runner| -> Vec<OsString> {
            match runner {
                Runner::ClaudeCode => super::super::claude_code::build_args(
                    "do the thing",
                    None,
                    PermissionGrant::Unrestricted,
                    Some(Path::new("/workspace/mcp-config.json")),
                ),
                Runner::Codex => super::super::codex::build_args(
                    Path::new("/workspace"),
                    Path::new("/workspace/outputs/final.md"),
                    None,
                    PermissionGrant::Unrestricted,
                    "do the thing",
                ),
                Runner::CursorAgent => super::super::cursor_agent::build_args(
                    Path::new("/workspace"),
                    None,
                    PermissionGrant::Unrestricted,
                    "do the thing",
                ),
            }
        };

        for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
            let argv = argv_for(runner);
            for control in HarnessControl::ALL {
                match runner.support(control) {
                    ControlSupport::Driven(ControlMechanism::Flag(flag)) => {
                        assert!(
                            argv.contains(&OsString::from(flag)),
                            "{} declares '{}' driven via {flag} but its built invocation does not carry it",
                            runner.display_name(),
                            control.label()
                        );
                    }
                    ControlSupport::Driven(ControlMechanism::GuardedFlag { value, guard }) => {
                        assert!(
                            argv.contains(&OsString::from(value)),
                            "{} declares '{}' driven via {value} but its built invocation does not carry it",
                            runner.display_name(),
                            control.label()
                        );
                        assert!(
                            argv.contains(&OsString::from(guard)),
                            "{} declares '{}' guarded by {guard} but its built invocation does not carry the guard",
                            runner.display_name(),
                            control.label()
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn the_matrix_and_config_home_agree() {
        use super::super::environment::HarnessConfigHome;

        for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
            let support = runner.support(HarnessControl::RunScopedConfigHome);
            match runner.config_home() {
                HarnessConfigHome::Redirectable { var, .. } => {
                    assert_eq!(support, ControlSupport::Driven(ControlMechanism::EnvVar(var)));
                }
                HarnessConfigHome::HomeRelative { .. } => {
                    assert_eq!(support, ControlSupport::Absent);
                }
            }
        }
    }

    #[test]
    fn the_matrix_and_pricing_agree() {
        use crate::agentskills::budget::HarnessPricing;

        for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
            let reports_cost =
                runner.support(HarnessControl::CostReporting) == ControlSupport::Driven(ControlMechanism::Reported);
            let publishes = matches!(runner.pricing(), HarnessPricing::Publishes);
            assert_eq!(reports_cost, publishes);
        }
    }

    #[test]
    fn unsupported_reason_names_the_harness_and_the_control() {
        assert_eq!(
            Runner::Codex.unsupported_reason(HarnessControl::ToolAllowlist),
            "the codex harness offers no tool allowlist"
        );
    }

    #[test]
    fn the_reference_table_matches_the_declaration() {
        const DOC: &str = include_str!("../../../docs/reference/ai-skills-eval.md");

        let section_start = DOC
            .find("## Harness support")
            .expect("the reference doc has a Harness support section");
        let section = &DOC[section_start..];
        let section_end = section.find("\n---").unwrap_or(section.len());
        let section = &section[..section_end];

        let table_lines: Vec<&str> = section
            .lines()
            .filter(|line| line.trim_start().starts_with('|'))
            .collect();
        assert!(
            table_lines.len() >= 2,
            "the Harness support section must contain a markdown table"
        );

        let header_cells = row_cells(table_lines[0]);
        assert_eq!(header_cells[1], "`claude-code`");
        assert_eq!(header_cells[2], "`codex`");
        assert_eq!(header_cells[3], "`cursor-agent`");

        let runners_in_column_order = [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent];

        let data_rows = &table_lines[2..];
        assert_eq!(data_rows.len(), HarnessControl::ALL.len());

        let mut seen_controls = Vec::new();
        for row in data_rows {
            let cells = row_cells(row);
            let control = HarnessControl::ALL
                .into_iter()
                .find(|control| control.label() == cells[0])
                .unwrap_or_else(|| panic!("'{}' is not a known harness control label", cells[0]));
            seen_controls.push(control);

            for (column, runner) in runners_in_column_order.iter().enumerate() {
                let expected = runner.support(control).describe();
                assert_eq!(
                    cells[column + 1],
                    expected,
                    "row '{}' column '{}' does not match the declaration",
                    control.label(),
                    runner.display_name()
                );
            }
        }

        for control in HarnessControl::ALL {
            assert!(
                seen_controls.contains(&control),
                "the reference table is missing a row for '{}'",
                control.label()
            );
        }
    }

    fn row_cells(line: &str) -> Vec<&str> {
        line.trim().trim_matches('|').split('|').map(str::trim).collect()
    }

    #[test]
    fn describe_renders_each_shape() {
        assert_eq!(ControlSupport::Absent.describe(), "no");
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::Flag("--force")).describe(),
            "`--force`"
        );
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::Subcommand("resume")).describe(),
            "`resume` subcommand"
        );
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::EnvVar("CODEX_HOME")).describe(),
            "`CODEX_HOME`"
        );
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::Reported).describe(),
            "reported"
        );
        assert_eq!(
            ControlSupport::Offered(ControlMechanism::Flag("--allowedTools")).describe(),
            "`--allowedTools` (harness only)"
        );
        assert_eq!(
            ControlSupport::Offered(ControlMechanism::Subcommand("resume")).describe(),
            "`resume` subcommand (harness only)"
        );
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::GuardedFlag {
                value: "--mcp-config",
                guard: "--strict-mcp-config",
            })
            .describe(),
            "`--mcp-config` (guarded by `--strict-mcp-config`)"
        );
    }

    #[test]
    fn guarded_flag_unwraps_its_value_and_guard() {
        let support = ControlSupport::Driven(ControlMechanism::GuardedFlag {
            value: "--mcp-config",
            guard: "--strict-mcp-config",
        });
        assert_eq!(support.flag(), Some("--mcp-config"));
        assert_eq!(support.guard_flag(), Some("--strict-mcp-config"));
        assert_eq!(
            ControlSupport::Driven(ControlMechanism::Flag("--force")).guard_flag(),
            None
        );
    }
}
