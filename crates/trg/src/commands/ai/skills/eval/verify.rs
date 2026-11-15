mod constants;
use constants::{GATE_FAILED, INVALID_SKILL, INVALID_SUITE};
use std::path::{Path, PathBuf};

use crate::agentskills::ci::{
    collect_case_scores_in_workspace, collect_failed_assertions_in_workspace, collect_workspace_metrics,
    emit_github_annotations, find_report_dir, print_human_summary, run_ci_checks, EvalCommandJsonOutput,
};
use crate::agentskills::evals::{
    check_eval_suite, check_workspace, lint_eval_suite_fixtures, print_eval_lint_warnings, EvalCheckOptions,
    EvalDirName, EvalLintOptions, WorkspaceCheckOptions,
};
use crate::agentskills::grading::telemetry::{eval_error_type, phase, phase_span, record_error};
use crate::agentskills::schemas::{validate_report_bundle_schemas, SchemaValidation};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
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

    fn requires_graders(self) -> bool {
        matches!(self, Self::Strict)
    }

    /// Why this build cannot honour the mode it was asked for, when it cannot.
    ///
    /// Strict mode's whole claim is that the bundle was held against the schemas that
    /// describe it. A build with the validator compiled out would pass every artifact
    /// without reading one and still exit clean, so the operator would take a verdict
    /// nobody reached. Refusing says which of the two happened.
    fn refusal(self, validation: SchemaValidation) -> Option<String> {
        match (self, validation) {
            (Self::Strict, SchemaValidation::Absent) => Some(
                "--mode strict cannot verify this bundle: this build of trg was compiled without the 'schema-validation' feature, so every artifact would pass unexamined and a clean exit would say nothing about the bundle. Rebuild with the feature to verify, or use --mode lenient and do not read its result as a schema check."
                    .to_string(),
            ),
            _ => None,
        }
    }
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval verify --workspace-dir ./artifacts/.../runs/run-001/workspace

  $ trg ai skills eval verify --workspace-dir ./workspace --mode strict

  $ trg ai skills eval verify --workspace-dir \"./path/with spaces/.../workspace\" --output-format json
")]
pub struct VerifyArgs {
    #[arg(long, help = "Path to the workspace directory containing grading.json / timing.json")]
    pub workspace_dir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Path to a skill directory containing evals/evals.json"
    )]
    pub skill_dir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "NAME",
        help = "Directory under --skill-dir the eval suite is resolved from (default: evals)"
    )]
    pub eval_dir: Option<EvalDirName>,

    #[arg(long, value_enum, default_value_t = VerifyMode::Lenient)]
    pub mode: VerifyMode,

    #[arg(long, help = "Fail when any eval case declares no grader")]
    pub require_graders: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,

    #[command(flatten)]
    pub ci: EvalCiArgs,
}

impl VerifyArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        if self.workspace_dir.is_none() && self.skill_dir.is_none() {
            eprintln!("Either --workspace-dir or --skill-dir is required");
            return ExitCode::InfrastructureFailure;
        }

        if let Some(refusal) = self.mode.refusal(SchemaValidation::of_this_build()) {
            eprintln!("{refusal}");
            return ExitCode::InfrastructureFailure;
        }

        if let Some(skill_dir) = &self.skill_dir {
            if let Some(code) = self.verify_skill_dir(fs, skill_dir) {
                return code;
            }
        }

        let Some(workspace) = self.workspace_dir else {
            return ExitCode::Success;
        };

        // A path that is not a bundle is not a bundle that failed its checks. There was
        // nothing to hold against them, so the answer is about the invocation rather than
        // about the skill. `check_workspace` reports both of these as validation failures,
        // which is how they would otherwise reach the verdict as findings about the skill.
        if !workspace.is_dir() {
            let reason = if workspace.exists() {
                "must be a directory"
            } else {
                "does not exist"
            };
            eprintln!(
                "Bundle verification failed: workspace '{}': {reason}",
                workspace.display()
            );
            return ExitCode::InfrastructureFailure;
        }

        let report_dir = find_report_dir(&workspace).unwrap_or_else(|| workspace.clone());

        // Whether a bundle conforms to the schemas is a fact about the writer that produced
        // it, not about how the suite scored, so it is read before the verdict. Behind the
        // verdict it was reachable only on a graded bundle whose every assertion passed,
        // which is a bundle no pass without a live runner can produce, so nothing ever held
        // a written bundle against the schemas that describe it.
        if matches!(self.mode, VerifyMode::Strict) {
            let span = phase_span!("validate schemas");
            if let Err(error) = phase(span, || validate_report_bundle_schemas(&report_dir)) {
                eprintln!("Schema validation failed: {error}");
                return ExitCode::GateFailed;
            }
        }

        let span = phase_span!("check workspace");
        let workspace_report = match phase(span, || check_workspace(&workspace, self.mode.into_workspace_options())) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Bundle verification failed: {}", e);
                return e.reported_as();
            }
        };

        let collect = phase_span!("collect results");
        let metrics = match phase(collect.clone(), || collect_workspace_metrics(&workspace)) {
            Ok(metrics) => metrics,
            Err(error) => {
                eprintln!("Failed to collect workspace metrics: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        let mut failed_assertions = Vec::new();
        if let Err(error) = phase(collect.clone(), || {
            collect_failed_assertions_in_workspace(
                &workspace,
                None,
                workspace.display().to_string(),
                &mut failed_assertions,
            )
        }) {
            eprintln!("Failed to collect failed assertions: {error}");
            return ExitCode::InfrastructureFailure;
        }

        let mut case_scores = Vec::new();
        if let Err(error) = phase(collect.clone(), || {
            collect_case_scores_in_workspace(&workspace, None, workspace.display().to_string(), &mut case_scores)
        }) {
            eprintln!("Failed to collect case scores: {error}");
            return ExitCode::InfrastructureFailure;
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

        let gate = phase_span!("run ci checks");
        let check = gate.in_scope(|| {
            run_ci_checks(
                &metrics,
                policy,
                &self.ci.thresholds(),
                &failed_assertions,
                &missing_grading,
                &case_scores,
            )
        });
        if !check.passed {
            record_error(&gate, GATE_FAILED);
        }
        drop(gate);
        tracing::info_span!("emit annotations").in_scope(|| emit_github_annotations(&check.violations));

        let exit_code = ExitCode::from_gate(check.passed);
        if self.output_format.is_json() {
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
                    return ExitCode::InfrastructureFailure;
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

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    fn verify_skill_dir(&self, fs: &impl FileSystem, skill_dir: &Path) -> Option<ExitCode> {
        let span = phase_span!("check skill");
        let _entered = span.enter();
        let validated = phase_span!("validate skill");
        let props = match validated.in_scope(|| crate::agentskills::validator::validate_skill(fs, skill_dir)) {
            Ok(props) => props,
            Err(error) => {
                record_error(&validated, INVALID_SKILL);
                record_error(&span, INVALID_SKILL);
                eprintln!("Skill validation failed: {error}");
                return Some(ExitCode::GateFailed);
            }
        };

        let require_graders = self.require_graders || self.mode.requires_graders();
        let eval_dir = self.eval_dir.clone().unwrap_or_default();
        let checked = phase_span!("check eval suite");
        if let Err(error) = phase(checked, || {
            check_eval_suite(
                fs,
                skill_dir,
                &eval_dir,
                &props.name,
                EvalCheckOptions {
                    require_graders,
                    ..EvalCheckOptions::default()
                },
            )
        }) {
            record_error(&span, INVALID_SUITE);
            eprintln!("Eval manifest verification failed: {error}");
            return Some(ExitCode::GateFailed);
        }

        let loaded = phase_span!("load eval suite");
        let suite = match phase(loaded, || {
            crate::agentskills::evals::load_eval_suite(fs, skill_dir, &eval_dir)
        }) {
            Ok(suite) => suite,
            Err(error) => {
                record_error(&span, eval_error_type(&error));
                eprintln!("Failed to load eval manifest: {error}");
                return Some(ExitCode::InfrastructureFailure);
            }
        };
        let warnings = tracing::info_span!("lint fixtures").in_scope(|| {
            lint_eval_suite_fixtures(
                fs,
                skill_dir,
                &suite,
                EvalLintOptions {
                    allow_empty_graders: require_graders,
                    ..EvalLintOptions::default()
                },
            )
        });
        print_eval_lint_warnings(&warnings);

        None
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
