use super::super::environment::HarnessConfigHome;
use super::*;
use crate::agentskills::budget::HarnessPricing;
use crate::agentskills::report::PermissionGrant;
use crate::agentskills::system_prompt_appendix::SystemPromptAppendix;
use crate::agentskills::tool_grant::ToolGrant;
use std::ffi::OsString;
use std::path::Path;

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
        ControlSupport::Driven(ControlMechanism::Flag("--allowedTools"))
    );
    assert_eq!(
        Runner::ClaudeCode.support(HarnessControl::TurnCap),
        ControlSupport::Driven(ControlMechanism::Flag("--max-turns"))
    );
    assert_eq!(
        Runner::ClaudeCode.support(HarnessControl::SystemPromptAppend),
        ControlSupport::Driven(ControlMechanism::Flag("--append-system-prompt"))
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
        ControlSupport::Driven(ControlMechanism::ConfigFile {
            home_var: "CODEX_HOME",
            file: "config.toml",
        })
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
    let tool_grant = ToolGrant::parse(["Bash"]).unwrap();
    let appendix = SystemPromptAppendix::parse("Answer in British English.").unwrap();
    let argv_for = |runner: Runner| -> Vec<OsString> {
        match runner {
            Runner::ClaudeCode => super::super::claude_code::build_args(
                "do the thing",
                None,
                PermissionGrant::Unrestricted,
                Some(&tool_grant),
                Some(Path::new("/workspace/mcp-config.json")),
                Some(&appendix),
                Some("3".parse().expect("a positive turn cap")),
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
    assert_eq!(
        ControlSupport::Driven(ControlMechanism::ConfigFile {
            home_var: "CODEX_HOME",
            file: "config.toml",
        })
        .describe(),
        "`config.toml` in `$CODEX_HOME`"
    );
}

/// A config file is not argv. Letting it answer `flag()` would have a runner splice
/// `config.toml` into the command line it builds, and the harness would refuse to start.
#[test]
fn a_config_file_mechanism_is_never_mistaken_for_something_to_put_on_the_command_line() {
    let support = ControlSupport::Driven(ControlMechanism::ConfigFile {
        home_var: "CODEX_HOME",
        file: "config.toml",
    });
    assert_eq!(support.flag(), None);
    assert_eq!(support.guard_flag(), None);
}

/// The whole reason the codex cell is driven only under an isolated environment. A
/// mechanism that answered `false` here would let trg add mocks beside the operator's
/// own MCP servers and report the run as if only the mocks had been reachable.
#[test]
fn only_a_config_file_mechanism_asks_for_a_config_home_the_run_owns() {
    assert!(Runner::Codex
        .support(HarnessControl::McpServers)
        .needs_run_owned_config_home());
    assert!(!Runner::ClaudeCode
        .support(HarnessControl::McpServers)
        .needs_run_owned_config_home());
    assert!(!ControlSupport::Absent.needs_run_owned_config_home());
    assert!(!ControlSupport::Offered(ControlMechanism::ConfigFile {
        home_var: "CODEX_HOME",
        file: "config.toml",
    })
    .needs_run_owned_config_home());
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
