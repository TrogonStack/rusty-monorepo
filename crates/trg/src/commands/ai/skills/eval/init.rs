use std::path::PathBuf;

use crate::agentskills::evals::{check_eval_suite, write_eval_manifest_scaffold, EvalCheckOptions, EvalDirName};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

use super::print_json;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval init --skill-dir ./skills/my-skill

Optional eval-level metadata in evals/evals.json:
  tags                  Categorize eval cases (e.g. smoke, regression)
  priority              low | normal | high | critical
  timeout_secs          Per-eval runner timeout override (overrides --timeout-secs)
  expected_output_files Paths under outputs/ the runner should produce (warn if missing)
  grader_hints          Opaque JSON passed through to script graders

Timeout precedence: per-eval timeout_secs > --timeout-secs > runner default (no limit).

The scaffold is a shape, not a measurement: a case that passes with and without the
skill measures nothing. To turn it into a suite that discriminates, follow
docs/how-to/author-an-eval-suite.md, or hand an agent the skill at
skills/trg-eval-authoring.

Once the suite discriminates, improving the skill one change per round is
docs/how-to/hillclimb-a-skill.md, or the skill at skills/trg-eval-hillclimb.
")]
pub struct InitArgs {
    #[arg(long, value_name = "DIR", help = "Path to a skill directory containing SKILL.md")]
    pub skill_dir: PathBuf,

    #[arg(
        long,
        value_name = "NAME",
        help = "Directory under the skill to scaffold the eval suite into (default: evals)"
    )]
    pub eval_dir: Option<EvalDirName>,

    #[arg(long, help = "Overwrite an existing evals/evals.json")]
    pub force: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl InitArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        let props = match crate::agentskills::validator::validate_skill(fs, &self.skill_dir) {
            Ok(props) => props,
            Err(error) => {
                eprintln!("Skill validation failed: {error}");
                return ExitCode::GateFailed;
            }
        };

        let eval_dir = self.eval_dir.unwrap_or_default();
        let evals_path = self.skill_dir.join(eval_dir.as_str()).join("evals.json");
        if fs.exists(&evals_path) && !self.force {
            eprintln!(
                "Refusing to overwrite existing eval manifest at {}",
                evals_path.display()
            );
            return ExitCode::InfrastructureFailure;
        }

        if let Err(error) = std::fs::create_dir_all(self.skill_dir.join(eval_dir.as_str())) {
            eprintln!("Failed to create evals directory: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if let Err(error) = write_eval_manifest_scaffold(fs, &self.skill_dir, &eval_dir, &props.name) {
            eprintln!("Failed to write eval manifest: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if let Err(error) = check_eval_suite(
            fs,
            &self.skill_dir,
            &eval_dir,
            &props.name,
            EvalCheckOptions {
                require_graders: true,
                ..EvalCheckOptions::default()
            },
        ) {
            eprintln!("Generated eval manifest failed validation: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            #[derive(Serialize)]
            struct EvalInitDocument {
                skill_dir: String,
                evals_path: String,
                created: bool,
            }
            let document = EvalInitDocument {
                skill_dir: self.skill_dir.display().to_string(),
                evals_path: evals_path.display().to_string(),
                created: true,
            };
            return print_json(&document, ExitCode::Success);
        }

        println!("Created {}", evals_path.display());
        ExitCode::Success
    }
}

#[cfg(test)]
#[path = "init_tests.rs"]
mod tests;
