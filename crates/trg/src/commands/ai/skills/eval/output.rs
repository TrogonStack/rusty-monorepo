use std::path::Path;

use serde::Serialize;

use crate::exit_code::ExitCode;

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub fn print_report_dir(dir: &Path) {
    println!("{}", dir.display());
}

/// Render one document on stdout, answering with the code the command should exit with.
///
/// `code` is the command's own verdict and is kept, since a pass that failed still has a
/// failure to report. Only a document that cannot be rendered changes it, and it changes
/// it to a broken tool rather than a failed gate: the command reached a verdict and then
/// handed the caller nothing to read it from, which says nothing about the skill.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
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
#[path = "output_tests.rs"]
mod tests;
