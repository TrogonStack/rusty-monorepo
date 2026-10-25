use std::path::PathBuf;

use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;
use serde_json::json;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ValidateArgs {
    #[arg(help = "Path to skill directory or SKILL.md file")]
    pub path: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the verdict as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl ValidateArgs {
    /// An invalid skill is an answer rather than a breakdown, so under `json`
    /// it is a document on stdout like any other verdict, and the exit code
    /// carries the same verdict for a caller that is not reading it.
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        let skill_path = resolve_skill_path(&self.path);
        let verdict = crate::agentskills::validator::validate_skill(fs, &skill_path);

        if self.output_format.is_json() {
            let (valid, code) = (verdict.is_ok(), ExitCode::from_success(verdict.is_ok()));
            let document = json!({
                "path": skill_path.display().to_string(),
                "valid": valid,
                "error": verdict.as_ref().err().map(ToString::to_string),
            });
            return print_json(&document, code);
        }

        match verdict {
            Ok(_) => {
                println!("✓ Skill is valid");
                ExitCode::Success
            }
            Err(e) => {
                eprintln!("✗ Validation failed: {}", e);
                ExitCode::Failed
            }
        }
    }
}
