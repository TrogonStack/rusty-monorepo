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
    RunScopedConfigHome,
    CostReporting,
}

impl HarnessControl {
    pub const ALL: [HarnessControl; 8] = [
        Self::ToolAllowlist,
        Self::TurnCap,
        Self::SystemPromptAppend,
        Self::McpServers,
        Self::SandboxLevels,
        Self::ConversationResume,
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
            Self::RunScopedConfigHome => "run-scoped config home",
            Self::CostReporting => "cost reporting",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ControlSupport {
    Flag(&'static str),
    Subcommand(&'static str),
    EnvVar(&'static str),
    Reported,
    Absent,
}

impl ControlSupport {
    pub fn is_offered(self) -> bool {
        !matches!(self, Self::Absent)
    }

    pub fn flag(self) -> Option<&'static str> {
        match self {
            Self::Flag(flag) => Some(flag),
            _ => None,
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Flag(flag) => format!("`{flag}`"),
            Self::Subcommand(subcommand) => format!("`{subcommand}` subcommand"),
            Self::EnvVar(var) => format!("`{var}`"),
            Self::Reported => "reported".to_string(),
            Self::Absent => "no".to_string(),
        }
    }
}

impl Runner {
    pub fn support(self, control: HarnessControl) -> ControlSupport {
        match (self, control) {
            (Self::ClaudeCode, HarnessControl::ToolAllowlist) => ControlSupport::Flag("--allowedTools"),
            (Self::ClaudeCode, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::ClaudeCode, HarnessControl::SystemPromptAppend) => ControlSupport::Flag("--append-system-prompt"),
            (Self::ClaudeCode, HarnessControl::McpServers) => ControlSupport::Flag("--mcp-config"),
            (Self::ClaudeCode, HarnessControl::SandboxLevels) => ControlSupport::Flag("--permission-mode"),
            (Self::ClaudeCode, HarnessControl::ConversationResume) => ControlSupport::Flag("--resume"),
            (Self::ClaudeCode, HarnessControl::RunScopedConfigHome) => ControlSupport::EnvVar("CLAUDE_CONFIG_DIR"),
            (Self::ClaudeCode, HarnessControl::CostReporting) => ControlSupport::Reported,

            (Self::Codex, HarnessControl::ToolAllowlist) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::SystemPromptAppend) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::McpServers) => ControlSupport::Absent,
            (Self::Codex, HarnessControl::SandboxLevels) => ControlSupport::Flag("-s"),
            (Self::Codex, HarnessControl::ConversationResume) => ControlSupport::Subcommand("resume"),
            (Self::Codex, HarnessControl::RunScopedConfigHome) => ControlSupport::EnvVar("CODEX_HOME"),
            (Self::Codex, HarnessControl::CostReporting) => ControlSupport::Absent,

            (Self::CursorAgent, HarnessControl::ToolAllowlist) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::TurnCap) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::SystemPromptAppend) => ControlSupport::Absent,
            (Self::CursorAgent, HarnessControl::McpServers) => ControlSupport::Absent,
            // cursor-agent accepts a sandbox level through --force, but it is the CLI's
            // only documented non-interactive grant: both permission grants collapse onto
            // this one flag rather than onto two distinct levels.
            (Self::CursorAgent, HarnessControl::SandboxLevels) => ControlSupport::Flag("--force"),
            (Self::CursorAgent, HarnessControl::ConversationResume) => ControlSupport::Flag("--resume"),
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
        assert_eq!(count, 24);

        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::ToolAllowlist),
            ControlSupport::Flag("--allowedTools")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::TurnCap),
            ControlSupport::Absent
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::SystemPromptAppend),
            ControlSupport::Flag("--append-system-prompt")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::McpServers),
            ControlSupport::Flag("--mcp-config")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::SandboxLevels),
            ControlSupport::Flag("--permission-mode")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::ConversationResume),
            ControlSupport::Flag("--resume")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::RunScopedConfigHome),
            ControlSupport::EnvVar("CLAUDE_CONFIG_DIR")
        );
        assert_eq!(
            Runner::ClaudeCode.support(HarnessControl::CostReporting),
            ControlSupport::Reported
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
            ControlSupport::Flag("-s")
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::ConversationResume),
            ControlSupport::Subcommand("resume")
        );
        assert_eq!(
            Runner::Codex.support(HarnessControl::RunScopedConfigHome),
            ControlSupport::EnvVar("CODEX_HOME")
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
            ControlSupport::Flag("--force")
        );
        assert_eq!(
            Runner::CursorAgent.support(HarnessControl::ConversationResume),
            ControlSupport::Flag("--resume")
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
    fn the_matrix_and_config_home_agree() {
        use super::super::environment::HarnessConfigHome;

        for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
            let support = runner.support(HarnessControl::RunScopedConfigHome);
            match runner.config_home() {
                HarnessConfigHome::Redirectable { var, .. } => {
                    assert_eq!(support, ControlSupport::EnvVar(var));
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
            let reports_cost = runner.support(HarnessControl::CostReporting) == ControlSupport::Reported;
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
        assert_eq!(ControlSupport::Flag("--force").describe(), "`--force`");
        assert_eq!(ControlSupport::Subcommand("resume").describe(), "`resume` subcommand");
        assert_eq!(ControlSupport::EnvVar("CODEX_HOME").describe(), "`CODEX_HOME`");
        assert_eq!(ControlSupport::Reported.describe(), "reported");
        assert_eq!(ControlSupport::Absent.describe(), "no");
    }
}
