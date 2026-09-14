use std::path::Path;

use serde::Serialize;

use crate::agentskills::exit_code::ExitCode;

pub fn print_report_dir(dir: &Path) {
    println!("{}", dir.display());
}

/// Render one document on stdout, answering with the code the command should exit with.
///
/// `code` is the command's own verdict and is kept, since a pass that failed still has a
/// failure to report. Only a document that cannot be rendered changes it, and it changes
/// it to a broken tool rather than a failed gate: the command reached a verdict and then
/// handed the caller nothing to read it from, which says nothing about the skill.
pub(crate) fn print_json<T: Serialize>(value: &T, code: ExitCode) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            println!("{rendered}");
            code
        }
        Err(e) => {
            eprintln!("could not render the result as json: {e}");
            ExitCode::InfrastructureFailure
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value serde refuses, so the render failure is real rather than simulated: a map
    /// keyed by something that is not a string has no JSON object to become.
    fn unrenderable() -> std::collections::BTreeMap<(u8, u8), u8> {
        std::collections::BTreeMap::from([((1, 2), 3)])
    }

    #[test]
    fn a_verdict_the_caller_can_read_is_the_verdict_the_command_reached() {
        for verdict in [ExitCode::Success, ExitCode::GateFailed, ExitCode::BudgetExhausted] {
            assert_eq!(print_json(&serde_json::json!({ "ok": true }), verdict), verdict);
        }
    }

    #[test]
    fn a_document_that_cannot_be_rendered_is_a_broken_tool_and_not_a_failed_gate() {
        let code = print_json(&unrenderable(), ExitCode::Success);

        assert_eq!(code, ExitCode::InfrastructureFailure);
        assert_ne!(
            code,
            ExitCode::GateFailed,
            "handing the caller nothing to read says nothing about the skill"
        );
    }

    /// The downgrade is not a masking. A pass that already failed its gate and then could
    /// not be rendered has two problems, and the one that stops anyone from reading the
    /// first is the one worth reporting.
    #[test]
    fn a_failed_gate_that_cannot_be_rendered_reports_the_render() {
        assert_eq!(
            print_json(&unrenderable(), ExitCode::GateFailed),
            ExitCode::InfrastructureFailure
        );
    }
}
