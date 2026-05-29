use std::path::{Path, PathBuf};

use crate::agentskills::ci::{
    collect_failed_assertions_in_workspace, collect_workspace_metrics, emit_github_annotations, find_report_dir,
    print_human_summary, run_ci_checks, EvalCommandJsonOutput,
};
use crate::agentskills::evals::{
    check_eval_suite, check_workspace, lint_eval_suite_fixtures, print_eval_lint_warnings, EvalCheckOptions,
    EvalLintOptions, WorkspaceCheckOptions,
};
use crate::agentskills::schemas::validate_report_bundle_schemas;
use crate::fs::FileSystem;
use clap::{Args, ValueEnum};

use super::ci_args::EvalCiArgs;
use super::print_report_dir;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum VerifyMode {
    /// Tolerate missing grading files and surface failed assertions without erroring.
    Lenient,
    /// Require at least one grading.json and fail on failed assertions.
    Strict,
}

impl VerifyMode {
    fn into_workspace_options(self) -> WorkspaceCheckOptions {
        match self {
            Self::Lenient => WorkspaceCheckOptions {
                require_grading: false,
                fail_on_failed_assertions: false,
            },
            Self::Strict => WorkspaceCheckOptions {
                require_grading: true,
                fail_on_failed_assertions: true,
            },
        }
    }

    fn requires_assertions(self) -> bool {
        matches!(self, Self::Strict)
    }
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval verify ./artifacts/.../runs/run-001/workspace

  $ trg ai skills eval verify ./workspace --mode strict

  $ trg ai skills eval verify \"./path/with spaces/.../workspace\" --json
")]
pub struct VerifyArgs {
    #[arg(help = "Path to the workspace directory containing grading.json / timing.json")]
    pub workspace: Option<PathBuf>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Path to a skill directory containing evals/evals.json"
    )]
    pub skill_dir: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = VerifyMode::Lenient)]
    pub mode: VerifyMode,

    #[arg(long, help = "Fail when any eval case has an empty assertions array")]
    pub require_assertions: bool,

    #[arg(long, help = "Emit machine-readable JSON output")]
    pub json: bool,

    #[command(flatten)]
    pub ci: EvalCiArgs,
}

impl VerifyArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        if self.workspace.is_none() && self.skill_dir.is_none() {
            eprintln!("Either WORKSPACE or --skill-dir is required");
            return 1;
        }

        if let Some(skill_dir) = &self.skill_dir {
            if let Some(code) = self.verify_skill_dir(fs, skill_dir) {
                return code;
            }
        }

        let Some(workspace) = self.workspace else {
            return 0;
        };

        let workspace_report = match check_workspace(&workspace, self.mode.into_workspace_options()) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Bundle verification failed: {}", e);
                return 1;
            }
        };

        let report_dir = find_report_dir(&workspace).unwrap_or_else(|| workspace.clone());

        if matches!(self.mode, VerifyMode::Strict) {
            if let Err(error) = validate_report_bundle_schemas(&report_dir) {
                eprintln!("Schema validation failed: {error}");
                return 1;
            }
        }

        let metrics = match collect_workspace_metrics(&workspace) {
            Ok(metrics) => metrics,
            Err(error) => {
                eprintln!("Failed to collect workspace metrics: {error}");
                return 1;
            }
        };

        let mut failed_assertions = Vec::new();
        if let Err(error) = collect_failed_assertions_in_workspace(
            &workspace,
            None,
            workspace.display().to_string(),
            &mut failed_assertions,
        ) {
            eprintln!("Failed to collect failed assertions: {error}");
            return 1;
        }

        let mut policy = self.ci.policy();
        if matches!(self.mode, VerifyMode::Strict) {
            policy.fail_on_failed_assertions = true;
            policy.fail_on_missing_grading = true;
        }

        let missing_grading = if policy.fail_on_missing_grading && workspace_report.grading_files == 0 {
            vec![workspace.display().to_string()]
        } else {
            Vec::new()
        };

        let check = run_ci_checks(
            &metrics,
            policy,
            &self.ci.thresholds(),
            &failed_assertions,
            &missing_grading,
        );
        emit_github_annotations(&check.violations);

        let exit_code = if check.passed { 0 } else { 1 };
        if self.json {
            let output = EvalCommandJsonOutput {
                report_dir: report_dir.display().to_string(),
                exit_code,
                check,
                workspace: Some(workspace_report),
            };
            match serde_json::to_string_pretty(&output) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize eval output: {error}");
                    return 1;
                }
            }
        } else {
            print_report_dir(&report_dir);
            print_human_summary(&check);
            println!("workspace: {}", workspace.display());
            println!("  grading files: {}", workspace_report.grading_files);
            println!("  timing files: {}", workspace_report.timing_files);
        }

        exit_code
    }

    fn verify_skill_dir(&self, fs: &impl FileSystem, skill_dir: &Path) -> Option<i32> {
        let props = match crate::agentskills::validator::validate_skill(fs, skill_dir) {
            Ok(props) => props,
            Err(error) => {
                eprintln!("Skill validation failed: {error}");
                return Some(1);
            }
        };

        let require_assertions = self.require_assertions || self.mode.requires_assertions();
        if let Err(error) = check_eval_suite(
            fs,
            skill_dir,
            &props.name,
            EvalCheckOptions {
                require_assertions,
                ..EvalCheckOptions::default()
            },
        ) {
            eprintln!("Eval manifest verification failed: {error}");
            return Some(1);
        }

        let suite = match crate::agentskills::evals::load_eval_suite(fs, skill_dir) {
            Ok(suite) => suite,
            Err(error) => {
                eprintln!("Failed to load eval manifest: {error}");
                return Some(1);
            }
        };
        print_eval_lint_warnings(&lint_eval_suite_fixtures(
            fs,
            skill_dir,
            &suite,
            EvalLintOptions {
                allow_empty_assertions: require_assertions,
                ..EvalLintOptions::default()
            },
        ));

        None
    }
}
