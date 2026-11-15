mod constants;
use constants::{GRADING_AUTO, GRADING_NONE, RUN_FAILED, STEP_FAILED};
use std::cell::OnceCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use opentelemetry::trace::TraceContextExt;
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::agentskills::budget::{
    Admission, CostCeiling, CostLedger, MaxToolCalls, MaxTurns, RunCaps, FAILURE_KIND_BUDGET,
};
use crate::agentskills::cache::{
    apply_cache_hit, compute_fixture_hash, record_completion, runner_kind_label, try_resolve_cache, CacheKey,
    CacheKeyInput, CacheOptions, ReuseKeyInput, PROMPT_CONTRACT_VERSION,
};
use crate::agentskills::case_selection::{CaseSelection, SplitArg};
use crate::agentskills::concurrency::RunConcurrency;
use crate::agentskills::eval_telemetry::{
    exit_error_type, record_attempt, record_cache_lookup, record_failure, record_run, record_workflow_duration,
    step_span, AttemptOutcome,
};
use crate::agentskills::evals::{
    effective_model, effective_model_source, effective_timeout_secs, missing_expected_output_warnings, EvalCase,
    EvalCheckOptions, EvalDirName, EvalSuite,
};
use crate::agentskills::layout::detect_next_iteration;
use crate::agentskills::mocks::{
    materialize_mock_set, resolve_mock_set, MaterializedMcpConfig, MockCallLogEntry, MockServerBinary, MockServerEnv,
    MockSet, MOCK_CALLS_LOG_NAME,
};
use crate::agentskills::model_name::ModelName;
use crate::agentskills::outputs::index_output_artifacts;
use crate::agentskills::permission_outcome::PermissionOutcome;
use crate::agentskills::report::{
    build_report_bundle, recompute_model_capture_status, recompute_scenario_summaries, write_report_bundle,
    BudgetReport, BuildReportOptions, EnvironmentPolicy, PermissionGrant, ReportBundle, RunNotStarted, RunRecord,
    ScenarioKind, SkillIntegrityReport, SkillStaging, WriteReportOptions,
};
use crate::agentskills::runner::capabilities::{ControlSupport, HarnessControl};
use crate::agentskills::runner::{
    availability, compute_skill_digest, detect_tampering, measure_transcript, EvalRunOutcome, EvalRunRequest, Runner,
    RunnerError, SkillDigest, TelemetryForwarding,
};
use crate::agentskills::sampling::AttemptCount;
use crate::agentskills::scenario_selection::ScenarioSelection;
use crate::agentskills::span_reference::SpanReference;
use crate::agentskills::tool_grant::{effective_tool_grant, ToolGrant};
use crate::agentskills::workspace_scaffold::ScaffoldPermission;
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use crate::telemetry::propagation::LaneParent;
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, GEN_AI_OPERATION_NAME, GEN_AI_WORKFLOW_NAME,
};
use crate::telemetry::semconv::trg::{
    EVAL_ATTEMPT, EVAL_ATTEMPT_OUTCOME, EVAL_ATTEMPT_TRANSIENT, EVAL_CACHE_HIT, EVAL_CACHE_RESTORED_BYTES,
    EVAL_CACHE_RESTORED_FILES, EVAL_CASE_ID, EVAL_CASE_SPLIT, EVAL_CONCURRENCY, EVAL_GRADING_STRATEGY, EVAL_ITERATION,
    EVAL_LANE, EVAL_MODEL_CONFIG, EVAL_RUNNER_KIND, EVAL_RUNNER_MODEL, EVAL_RUNNER_VERSION, EVAL_RUN_COUNT,
    EVAL_RUN_ID, EVAL_RUN_STATUS, EVAL_SCENARIO, EVAL_SCENARIOS, EVAL_SKILL_REVISION, EVAL_SKILL_TAMPERED,
    EVAL_TOOL_GRANT,
};
use clap::Args;

use super::benchmark::benchmark_report_dir_with_document;
use super::ci_args::EvalCiArgs;
use super::grade::{grade_report_dir_with_report, GradeJsonOutput};
use super::{finish_eval_output, print_json};
use crate::agentskills::benchmark::BenchmarkOptions;
use crate::agentskills::grading::{GradeOptions, GraderMode};
use crate::agentskills::transcript::{read_normalized_transcript, NormalizedTranscript};

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval run --skill-dir ./skills/my-skill --out-dir ./artifacts

  $ trg ai skills eval run --skill-dir ./skills/my-skill --out-dir ./artifacts \\
      --runner cursor-agent --scenario with_skill --scenario without_skill

  $ trg ai skills eval run --skill-dir \"./skills/my skill\" --out-dir ./artifacts --force

  # Run, grade, and benchmark in one go (typical CI invocation)
  $ trg ai skills eval run --skill-dir ./my-skill --runner codex --grade --benchmark
")]
pub struct RunArgs {
    #[arg(long, value_name = "DIR", help = "Path to a skill directory containing SKILL.md")]
    pub skill_dir: PathBuf,

    #[arg(
        long,
        value_name = "NAME",
        help = "Directory under the skill, holding the eval suite (default: evals). Wins over a conflicting eval_dir declared in the manifest itself, since the manifest cannot be found before this is settled"
    )]
    pub eval_dir: Option<EvalDirName>,

    #[arg(long, value_name = "DIR", help = "Root directory for the generated artifact bundle")]
    pub out_dir: PathBuf,

    #[arg(
        long,
        value_name = "LABEL",
        default_value = "ci-default",
        help = "Opaque model configuration label recorded in report.json"
    )]
    pub model_config: String,

    #[arg(
        long,
        value_enum,
        value_name = "KIND",
        help = "Scenario kind to include (repeatable). Unset runs with_skill and without_skill together, so the pass has a baseline to compare against; under --reuse-completed, unset runs with_skill alone"
    )]
    pub scenario: Vec<ScenarioKind>,

    #[arg(
        long = "case",
        value_name = "PATTERN",
        help = "Cover only the cases whose id matches this glob (* and ?), matched in full (repeatable)"
    )]
    pub cases: Vec<String>,

    #[arg(
        long = "tag",
        value_name = "TAG",
        help = "Cover only the cases carrying this tag (repeatable). Combined with --case it narrows further: named and tagged"
    )]
    pub tags: Vec<String>,

    #[arg(
        long = "split",
        value_enum,
        default_value_t = SplitArg::All,
        help = "Cover only the cases declaring this split. Combined with --case and --tag it narrows further"
    )]
    pub split: SplitArg,

    #[arg(
        long,
        value_enum,
        value_name = "RUNNER",
        help = "Agent CLI to execute each (eval × scenario). When unset, runs are scaffolded with status: skipped."
    )]
    pub runner: Option<Runner>,

    #[arg(
        long,
        value_name = "MODEL",
        help = "Optional model identifier forwarded to the runner CLI (--model/-m). A case that names its own model overrides this. When neither is set, the runner CLI picks its own default; CLI-specific string."
    )]
    pub runner_model: Option<ModelName>,

    #[arg(
        long,
        value_name = "N",
        help = "Per-run timeout in seconds. When exceeded, the runner subprocess is killed and the run is marked timeout."
    )]
    pub timeout_secs: Option<u64>,

    #[arg(
        long,
        value_name = "N",
        default_value_t = 0,
        help = "Retry transient runner failures (non-zero exit without result event, or timeout) up to N times"
    )]
    pub retries: u32,

    #[arg(long, help = "Overwrite an existing report directory if it already exists")]
    pub force: bool,

    #[arg(
        long,
        value_name = "N",
        help = "Iteration number for this eval run. When omitted, uses the next available iteration."
    )]
    pub iteration: Option<u32>,

    /// Left unset rather than defaulted so a case that pins its own count can be told apart
    /// from one taking the pass default: a count defaulted here would read as a choice the
    /// operator made and would silently override every case.
    #[arg(
        long,
        value_name = "N",
        help = "Draw each (eval case × scenario) this many times; attempt numbers run 1..N within one iteration. Overrides any count a case pinned. Defaults to 3, which is what makes a score a measurement rather than a sample of size one"
    )]
    pub attempts: Option<AttemptCount>,

    #[arg(
        short = 'j',
        long,
        value_name = "N",
        default_value_t = RunConcurrency::serial(),
        help = "Execute this many runs at once, 1 to 8. Cuts wall clock, not cost: every run still pays for its own model calls, and the lanes share one account's rate limit"
    )]
    pub concurrency: RunConcurrency,

    #[arg(
        long,
        value_name = "USD",
        help = "Refuse to start further runs once the pass has spent this many US dollars. The ledger is checked before each run starts, so a pass can still overshoot by whatever the runs already in flight cost. Only the claude-code runner reports cost today, so this ceiling never binds a codex or cursor-agent pass"
    )]
    pub max_cost_usd: Option<CostCeiling>,

    #[arg(
        long,
        value_name = "N",
        help = "Stop a run once it makes more than N tool calls and record it as budget_exhausted. Every runner is stopped by trg killing the harness when its stream shows the call that went past the cap"
    )]
    pub max_tool_calls_per_run: Option<MaxToolCalls>,

    #[arg(
        long,
        value_name = "N",
        help = "Stop a run once it takes more than N model turns and record it as budget_exhausted. claude-code enforces this itself through --max-turns; codex and cursor-agent are killed when their stream shows the turn that went past the cap, and codex turns are inferred from the gaps between its tool calls"
    )]
    pub max_turns_per_run: Option<MaxTurns>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Path to a previous skill directory for --scenario old_skill comparisons"
    )]
    pub old_skill_dir: Option<PathBuf>,

    #[arg(
        long,
        help = "Allow --old-skill-dir to use a different skill name than the current skill (default: names must match)"
    )]
    pub allow_skill_name_mismatch: bool,

    #[arg(
        long,
        help = "After a successful run, grade the report directory (same as `eval grade <report_dir>` with default grader flags)"
    )]
    pub grade: bool,

    #[arg(
        long,
        help = "After run (and grade when --grade is set), write benchmark.json. Without --grade, assertion stats are omitted and completed runs count as missing_grading in the benchmark"
    )]
    pub benchmark: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the final pipeline stage as a human summary or as a machine-readable document (benchmark if --benchmark, else grade if --grade, else run CI summary). Intermediate stages are not emitted"
    )]
    pub output_format: OutputFormat,

    #[arg(long, help = "Fail when any eval case declares no grader")]
    pub require_graders: bool,

    #[arg(long, help = "Print eval manifest lint warnings to stderr")]
    pub lint_evals: bool,

    #[arg(long, help = "Disable eval run caching and always execute the runner")]
    pub no_cache: bool,

    #[arg(
        long,
        help = "Reuse any prior completed run for the same eval case and scenario, even when the model config or runner differs (still invalidated when skill, evals, or fixtures change). The scenario is never forgotten: the arms of a comparison differ in nothing else. Rejected when more than one --scenario is requested"
    )]
    pub reuse_completed: bool,

    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        default_value_t = SkillStaging::Copy,
        help = "How to stage the skill into run workspaces: copy (default) keeps every path a run can follow inside the workspace; symlink is cheaper but tells the run where the skill really lives, and the eval suite sits next to it"
    )]
    pub skill_staging: SkillStaging,

    #[arg(
        long,
        value_enum,
        value_name = "POLICY",
        default_value_t = EnvironmentPolicy::Isolated,
        help = "How much of this machine each run may see: isolated (default) gives the run its own HOME and harness config home, so installed skills, global instructions, and MCP servers cannot reach it; scrubbed replaces the environment with an allowlist but leaves the host's HOME and config home in place; inherited passes the environment through"
    )]
    pub environment: EnvironmentPolicy,

    #[arg(
        long,
        value_enum,
        value_name = "GRANT",
        default_value_t = PermissionGrant::WorkspaceWrite,
        help = "How much a run's harness may do without prompting: workspace_write (default) lets it write inside its workspace and nothing wider; unrestricted lets it act anywhere. Translated into the harness's own flag, so a score never depends on the operator's saved permission settings"
    )]
    pub permission: PermissionGrant,

    #[arg(
        long,
        value_delimiter = ',',
        value_name = "TOOL",
        help = "The operator's ceiling on which tools a run's harness may reach for, as a comma-separated list of tool names. A case can narrow this further but never widen it. Unset means no ceiling from the operator; a harness with no equivalent control fails its runs as unsupported rather than silently ignoring this"
    )]
    pub allowed_tools: Vec<String>,

    #[arg(
        long,
        help = "Run the workspace scaffold a case declares. The script is author-supplied code that runs with your own reach, so a case that declares one fails its runs until this is passed"
    )]
    pub allow_scaffold: bool,

    #[arg(
        long,
        help = "Run a skill directory from outside this working tree without being asked about it. Its prompts decide what an agent does with your own credentials, so a foreign directory is asked about once and remembered; pass this where there is nobody to ask, such as CI"
    )]
    pub trust_skill: bool,

    #[arg(
        long,
        help = "Hand each run's harness trg's own OpenTelemetry settings, so a harness that exports telemetry sends its spans to the same collector under this pass's trace. Off by default because it lets a run reach a collector its --environment would otherwise keep from it"
    )]
    pub forward_telemetry: bool,

    #[command(flatten)]
    pub ci: EvalCiArgs,
}

impl RunArgs {
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        let started = Instant::now();
        let suite = SuiteSpan::start(self.grade, self.concurrency);
        let code = suite
            .span
            .in_scope(|| self.run_suite(fs, &suite))
            .unwrap_or_else(|code| code);
        suite.finish(started.elapsed(), code);
        code
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    fn run_suite(self, fs: &impl FileSystem, suite: &SuiteSpan) -> Result<ExitCode, ExitCode> {
        let props = step("validate skill", || {
            crate::agentskills::validator::validate_skill(fs, &self.skill_dir).map_err(|e| {
                eprintln!("Skill validation failed: {}", e);
                ExitCode::GateFailed
            })
        })?;
        suite.named(&props.name);

        let eval_dir = self.eval_dir.clone().unwrap_or_default();

        step("check suite", || {
            crate::agentskills::evals::check_eval_suite(
                fs,
                &self.skill_dir,
                &eval_dir,
                &props.name,
                EvalCheckOptions {
                    require_graders: self.require_graders,
                    ..EvalCheckOptions::default()
                },
            )
            .map_err(|e| {
                eprintln!("Skill eval validation failed: {}", e);
                ExitCode::GateFailed
            })
        })?;

        if self.lint_evals {
            step("lint fixtures", || {
                match crate::agentskills::evals::load_eval_suite(fs, &self.skill_dir, &eval_dir) {
                    Ok(suite) => {
                        crate::agentskills::evals::print_eval_lint_warnings(
                            &crate::agentskills::evals::lint_eval_suite_fixtures(
                                fs,
                                &self.skill_dir,
                                &suite,
                                crate::agentskills::evals::EvalLintOptions {
                                    allow_empty_graders: self.require_graders,
                                    ..crate::agentskills::evals::EvalLintOptions::default()
                                },
                            ),
                        );
                        Ok(())
                    }
                    Err(error) => {
                        eprintln!("Failed to load eval manifest for linting: {error}");
                        Err(ExitCode::InfrastructureFailure)
                    }
                }
            })?;
        }

        if !self.skill_staging.withholds_the_answer_key() {
            eprintln!(
                "Warning: --skill-staging {} stages links into the live skill directory, so a run that follows one reaches the eval suite next to it and can be scored on text it copied. Use the default --skill-staging copy for a score that rules that out.",
                self.skill_staging.as_str()
            );
        }

        let iteration = step("detect iteration", || {
            Ok(self
                .iteration
                .unwrap_or_else(|| detect_next_iteration(&self.out_dir, &props.name)))
        })?;
        suite.span.record(EVAL_ITERATION, i64::from(iteration));

        let scenario_selection = step("resolve scenarios", || {
            let selection = ScenarioSelection::resolve(&self.scenario, self.reuse_completed).map_err(|error| {
                eprintln!("{error}");
                ExitCode::InfrastructureFailure
            })?;
            if selection.contains(ScenarioKind::OldSkill) && self.old_skill_dir.is_none() {
                eprintln!("--old-skill-dir is required when --scenario old_skill is included");
                return Err(ExitCode::InfrastructureFailure);
            }
            Ok(selection)
        })?;
        suite.scenarios(scenario_selection.scenarios());

        if let Some(old_skill_dir) = &self.old_skill_dir {
            step(
                "validate old skill",
                || match crate::agentskills::validator::validate_skill(fs, old_skill_dir) {
                    Ok(old_props) => {
                        if old_props.name != props.name && !self.allow_skill_name_mismatch {
                            eprintln!(
                                "Old skill name '{}' does not match current skill name '{}' (pass --allow-skill-name-mismatch to override)",
                                old_props.name, props.name
                            );
                            return Err(ExitCode::InfrastructureFailure);
                        }
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("Old skill validation failed: {}", e);
                        Err(ExitCode::GateFailed)
                    }
                },
            )?;
        }

        let runner_probe = match self.runner {
            Some(runner) if !cfg!(test) => step("check runner availability", || {
                availability::check_runner_available(runner)
                    .map(Some)
                    .map_err(|unavailable| {
                        availability::eprint_runner_unavailable(&unavailable);
                        ExitCode::InfrastructureFailure
                    })
            })?,
            _ => None,
        };

        let cost_ledger = match self.runner {
            Some(runner) => Some(step("open cost ledger", || {
                CostLedger::open(self.max_cost_usd, runner.pricing()).map_err(|error| {
                    eprintln!("{error}");
                    ExitCode::InfrastructureFailure
                })
            })?),
            None => None,
        };

        let cases = CaseSelection::parse(&self.cases, &self.tags, self.split.narrowing()).map_err(|error| {
            eprintln!("{error}");
            ExitCode::InfrastructureFailure
        })?;

        let allowed_tools = if self.allowed_tools.is_empty() {
            None
        } else {
            Some(ToolGrant::parse(self.allowed_tools.clone()).map_err(|error| {
                eprintln!("--allowed-tools: {error}");
                ExitCode::InfrastructureFailure
            })?)
        };

        let build_options = BuildReportOptions {
            iteration: Some(iteration),
            attempts: self.attempts,
            old_skill_path: self.old_skill_dir.clone(),
            user_old_skill_path: self.old_skill_dir.clone(),
            runner: self.runner.map(Runner::display_name).map(str::to_string),
            runner_binary: runner_probe
                .as_ref()
                .map(|probe| probe.binary_path.to_string_lossy().into_owned()),
            runner_version: runner_probe.as_ref().and_then(|probe| probe.version.clone()),
            skill_staging: self.skill_staging,
            environment: self.environment,
            permission: PermissionOutcome::new(
                self.permission,
                self.runner.map_or(self.permission, |runner| {
                    runner.effective_permission_grant(self.permission)
                }),
            )
            .expect("a runner's effective grant is never narrower than what was requested"),
            allowed_tools: allowed_tools.clone(),
            cases,
            eval_dir,
            telemetry_forwarding: TelemetryForwarding::requested(self.forward_telemetry),
            ..BuildReportOptions::default()
        };

        step("admit skill", || {
            let operator = crate::agentskills::skill_trust::SkillTrust::granted(self.trust_skill);
            for dir in std::iter::once(&self.skill_dir).chain(self.old_skill_dir.iter()) {
                if let Err(e) = crate::agentskills::skill_trust::admit(dir, operator) {
                    eprintln!("{e}");
                    return Err(ExitCode::GateFailed);
                }
            }
            Ok(())
        })?;

        let mut bundle = step("build report bundle", || {
            build_report_bundle(
                fs,
                &self.skill_dir,
                &self.skill_dir,
                &props.name,
                &self.model_config,
                scenario_selection.scenarios(),
                build_options,
            )
            .map_err(|e| {
                eprintln!("Failed to build eval report bundle: {}", e);
                ExitCode::InfrastructureFailure
            })
        })?;
        suite
            .span
            .record(EVAL_SKILL_REVISION, bundle.document.suite.skill_hash.as_str());
        bundle.document.report.trace = SpanReference::of(&suite.span);

        let report_dir = step("write report bundle", || {
            write_report_bundle(
                &self.out_dir,
                &bundle,
                WriteReportOptions {
                    force: self.force,
                    iteration,
                },
            )
            .map_err(|e| {
                eprintln!("Failed to write eval report bundle: {}", e);
                ExitCode::InfrastructureFailure
            })
        })?;

        let mut budget_exhausted = false;
        if let (Some(runner), Some(cost_ledger)) = (self.runner, cost_ledger.as_ref()) {
            let cache_options = CacheOptions {
                enabled: !self.no_cache,
                reuse_completed: self.reuse_completed,
            };
            let runs_skipped = execute_runs(
                runner,
                self.runner_model.as_ref(),
                self.timeout_secs,
                self.retries,
                &self.skill_dir,
                self.old_skill_dir.as_deref(),
                &self.out_dir,
                &report_dir,
                bundle,
                cache_options,
                self.skill_staging,
                self.environment,
                self.permission,
                allowed_tools,
                ScaffoldPermission::granted(self.allow_scaffold),
                TelemetryForwarding::requested(self.forward_telemetry),
                self.concurrency,
                cost_ledger,
                RunCaps {
                    tool_calls: self.max_tool_calls_per_run,
                    turns: self.max_turns_per_run,
                },
            )?;
            budget_exhausted = runs_skipped > 0 || cost_ledger.overspent();
        }

        let grade_options = GradeOptions {
            grader: GraderMode::Auto,
            grader_provider: crate::agentskills::judge::JudgeProvider::default(),
            grader_model: None,
            grader_command: None,
            grader_votes: crate::agentskills::judge_votes::JudgeVotes::single(),
            strict: false,
        };

        if self.grade {
            let (code, grade_report) = grade_report_dir_with_report(&report_dir, grade_options, self.output_format);
            let last_stage = !code.is_success() || !self.benchmark;
            if last_stage {
                let exit_code = code.or_budget_exhausted(budget_exhausted);
                if self.output_format.is_json() {
                    return Ok(print_json(
                        &GradeJsonOutput::new(&report_dir, exit_code, grade_report.as_ref()),
                        exit_code,
                    ));
                }
                return Ok(exit_code);
            }
        }

        if self.benchmark {
            let (code, benchmark_doc) = benchmark_report_dir_with_document(&report_dir, BenchmarkOptions::default());
            let exit_code = code.or_budget_exhausted(budget_exhausted);
            if self.output_format.is_json() {
                if let Some(document) = benchmark_doc {
                    return Ok(print_json(
                        &BenchmarkJsonOutput::new(&report_dir, exit_code, &document),
                        exit_code,
                    ));
                }
            }
            return Ok(exit_code);
        }

        step("finish suite", || {
            Ok(finish_eval_output(
                &report_dir,
                self.output_format,
                self.ci.policy(),
                &self.ci.thresholds(),
                None,
                budget_exhausted,
            ))
        })
    }
}

/// The `invoke_workflow` span one pass runs under.
struct SuiteSpan {
    span: tracing::Span,
    workflow: OnceCell<String>,
}

impl SuiteSpan {
    fn start(grade: bool, concurrency: RunConcurrency) -> Self {
        let grading = if grade { GRADING_AUTO } else { GRADING_NONE };
        let span = tracing::info_span!(
            "invoke_workflow",
            "otel.status_code" = Empty,
            { GEN_AI_OPERATION_NAME } = gen_ai_operation_name::INVOKE_WORKFLOW,
            { GEN_AI_WORKFLOW_NAME } = Empty,
            { EVAL_SKILL_REVISION } = Empty,
            { EVAL_ITERATION } = Empty,
            { EVAL_SCENARIOS } = Empty,
            { EVAL_GRADING_STRATEGY } = grading,
            { EVAL_CONCURRENCY } = i64::try_from(concurrency.lanes()).unwrap_or(i64::MAX),
            { ERROR_TYPE } = Empty,
        );
        Self {
            span,
            workflow: OnceCell::new(),
        }
    }

    fn named(&self, workflow: &str) {
        self.span
            .context()
            .span()
            .update_name(format!("{} {workflow}", gen_ai_operation_name::INVOKE_WORKFLOW));
        self.span.record(GEN_AI_WORKFLOW_NAME, workflow);
        let _ = self.workflow.set(workflow.to_string());
    }

    fn scenarios(&self, scenarios: &[ScenarioKind]) {
        let names = scenarios.iter().map(|scenario| scenario.as_str()).collect::<Vec<_>>();
        self.span.record(EVAL_SCENARIOS, names.join(",").as_str());
    }

    fn finish(&self, elapsed: std::time::Duration, code: ExitCode) {
        if let Some(error_type) = exit_error_type(code) {
            record_failure(&self.span, error_type);
        }
        record_workflow_duration(self.workflow.get().map(String::as_str), elapsed, code);
    }
}

/// Runs one setup or teardown step of a pass in its own span, marking it failed when the
/// step stops the pass.
fn step<T>(name: &'static str, work: impl FnOnce() -> Result<T, ExitCode>) -> Result<T, ExitCode> {
    let span = step_span(name);
    let result = span.in_scope(work);
    if let Some(error_type) = result.as_ref().err().and_then(|code| exit_error_type(*code)) {
        record_failure(&span, error_type);
    }
    result
}

/// The chained-benchmark shape, alongside the chained-grade shape that
/// `grade` and `run --grade` share.
#[derive(serde::Serialize)]
struct BenchmarkJsonOutput<'a> {
    report_dir: String,
    exit_code: ExitCode,
    benchmark: &'a crate::agentskills::benchmark::BenchmarkDocument,
}

impl<'a> BenchmarkJsonOutput<'a> {
    fn new(
        report_dir: &Path,
        exit_code: ExitCode,
        benchmark: &'a crate::agentskills::benchmark::BenchmarkDocument,
    ) -> Self {
        Self {
            report_dir: report_dir.display().to_string(),
            exit_code,
            benchmark,
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn execute_runs(
    runner: Runner,
    runner_model: Option<&ModelName>,
    timeout_secs: Option<u64>,
    retries: u32,
    skill_path: &Path,
    old_skill_path: Option<&Path>,
    out_dir: &Path,
    report_dir: &Path,
    mut bundle: ReportBundle,
    cache_options: CacheOptions,
    skill_staging: SkillStaging,
    environment: EnvironmentPolicy,
    permission: PermissionGrant,
    allowed_tools: Option<ToolGrant>,
    scaffold_permission: ScaffoldPermission,
    telemetry_forwarding: TelemetryForwarding,
    concurrency: RunConcurrency,
    cost_ledger: &CostLedger,
    caps: RunCaps,
) -> std::result::Result<usize, ExitCode> {
    let eval_dir = bundle.document.suite.eval_dir.clone();
    let (skill_md, old_skill_md, suite) = step("load run inputs", || {
        let skill_md = std::fs::read_to_string(skill_path.join("SKILL.md")).map_err(|e| {
            eprintln!("Failed to read SKILL.md: {}", e);
            ExitCode::InfrastructureFailure
        })?;

        let old_skill_md = match old_skill_path {
            Some(old_path) => Some(std::fs::read_to_string(old_path.join("SKILL.md")).map_err(|e| {
                eprintln!("Failed to read old skill SKILL.md: {}", e);
                ExitCode::InfrastructureFailure
            })?),
            None => None,
        };

        let suite: EvalSuite =
            crate::agentskills::case_directories::resolve_eval_suite(&crate::fs::RealFS, skill_path, &eval_dir)
                .map(|compiled| compiled.suite)
                .map_err(|err| {
                    eprintln!("Failed to load eval suite: {}", err);
                    ExitCode::InfrastructureFailure
                })?;
        Ok((skill_md, old_skill_md, suite))
    })?;

    let case_index: HashMap<String, &EvalCase> = suite.evals.iter().map(|c| (c.id.to_string(), c)).collect();
    let runner_version = bundle.document.report.runner_version.clone();
    let runner_kind = runner_kind_label(runner).to_string();

    let execution = RunExecution {
        runner,
        runner_model,
        timeout_secs,
        retries,
        skill_path,
        old_skill_path,
        out_dir,
        report_dir,
        cache_options,
        skill_staging,
        environment,
        permission,
        allowed_tools,
        scaffold_permission,
        telemetry_forwarding,
        skill_md: &skill_md,
        old_skill_md: old_skill_md.as_deref(),
        case_index: &case_index,
        runner_version,
        runner_kind,
        skill_hash: bundle.document.suite.skill_hash.clone(),
        old_skill_hash: bundle.document.suite.old_skill_hash.clone(),
        evals_hash: bundle.document.suite.evals_hash.clone(),
        eval_dir,
        cost_ledger,
        caps,
    };
    tracing::info_span!(
        "eval step",
        "otel.name" = "execute runs",
        { EVAL_CONCURRENCY } = i64::try_from(concurrency.lanes()).unwrap_or(i64::MAX),
        { EVAL_RUN_COUNT } = i64::try_from(bundle.document.runs.len()).unwrap_or(i64::MAX),
    )
    .in_scope(|| execution.execute_all(&mut bundle.document.runs, concurrency));

    step("rewrite report", || {
        recompute_scenario_summaries(&mut bundle.document);
        recompute_model_capture_status(&mut bundle.document);

        let runs_skipped = bundle
            .document
            .runs
            .iter()
            .filter(|run| run.failure_kind.as_deref() == Some(FAILURE_KIND_BUDGET))
            .count();
        bundle.document.budget = Some(BudgetReport {
            ceiling_usd: cost_ledger.ceiling().map(CostCeiling::usd),
            spent: cost_ledger.spend(),
            exhausted: cost_ledger.exhausted(),
            runs_skipped,
        });

        let report_json = serde_json::to_string_pretty(&bundle.document).map_err(|e| {
            eprintln!("Failed to re-serialize report.json: {}", e);
            ExitCode::InfrastructureFailure
        })?;
        std::fs::write(report_dir.join("report.json"), report_json).map_err(|e| {
            eprintln!("Failed to write updated report.json: {}", e);
            ExitCode::InfrastructureFailure
        })?;

        Ok(runs_skipped)
    })
}

/// When the skill directory is hashed to see whether a run rewrote it.
///
/// Every run of a pass is handed the same skill directory, so a change to it can only be
/// pinned on one run while that run is the only thing executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityWindow {
    /// Hash around the run itself, which is the only candidate for what changed.
    Run,
    /// Leave the check to the pass, because the lanes share the directory and a change
    /// seen inside one run's window may have come from any of the others.
    Pass,
}

/// Everything the execution of one run needs that does not belong to that run.
///
/// Shared by reference across lanes, so nothing here may be specific to a single run.
struct RunExecution<'a> {
    runner: Runner,
    runner_model: Option<&'a ModelName>,
    timeout_secs: Option<u64>,
    retries: u32,
    skill_path: &'a Path,
    old_skill_path: Option<&'a Path>,
    out_dir: &'a Path,
    report_dir: &'a Path,
    cache_options: CacheOptions,
    skill_staging: SkillStaging,
    environment: EnvironmentPolicy,
    permission: PermissionGrant,
    allowed_tools: Option<ToolGrant>,
    scaffold_permission: ScaffoldPermission,
    telemetry_forwarding: TelemetryForwarding,
    skill_md: &'a str,
    old_skill_md: Option<&'a str>,
    case_index: &'a HashMap<String, &'a EvalCase>,
    runner_version: Option<String>,
    runner_kind: String,
    skill_hash: String,
    old_skill_hash: Option<String>,
    evals_hash: String,
    eval_dir: EvalDirName,
    cost_ledger: &'a CostLedger,
    caps: RunCaps,
}

impl RunExecution<'_> {
    /// Work through every run, up to `concurrency` of them at a time.
    ///
    /// Each lane takes the next run that nobody has claimed, rather than being handed a
    /// fixed share, because runs are not equally long: one case can take minutes while
    /// the next is served from cache, and a fixed share would leave lanes idle behind the
    /// slowest one. Runs keep their order in the report either way, since a lane writes
    /// only into the run it claimed.
    fn execute_all(&self, runs: &mut [RunRecord], concurrency: RunConcurrency) {
        let lanes = concurrency.lanes_for(runs.len());
        if concurrency.is_serial() || lanes <= 1 {
            for run in runs.iter_mut() {
                self.execute(run, IntegrityWindow::Run, Lane::SERIAL);
            }
            return;
        }

        let baseline = step_span("hash skills").in_scope(|| self.skill_digests());
        {
            let queue = &Mutex::new(runs.iter_mut().collect::<VecDeque<&mut RunRecord>>());
            thread::scope(|scope| {
                for lane in (0..lanes).map(Lane) {
                    let lane_context = lane_context();
                    let parent = LaneParent::capture();
                    scope.spawn(move || {
                        adopt_lane_context(lane_context);
                        parent.in_scope(|| loop {
                            #[expect(
                                clippy::disallowed_methods,
                                reason = "the lock is only ever held across `pop_front`, which cannot leave the queue half-written"
                            )]
                            let next = queue
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .pop_front();
                            match next {
                                Some(run) => self.execute(run, IntegrityWindow::Pass, lane),
                                None => break,
                            }
                        });
                    });
                }
            });
        }
        step_span("check skill tampering").in_scope(|| self.record_pass_integrity(runs, &baseline));
    }

    /// The directory whose contents answer for a scenario's skill.
    fn integrity_source(&self, scenario: ScenarioKind) -> Option<&Path> {
        match scenario {
            ScenarioKind::OldSkill => self.old_skill_path,
            _ => Some(self.skill_path),
        }
    }

    /// Hash every skill directory the pass will hand out, once, before any run starts.
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    fn skill_digests(&self) -> HashMap<PathBuf, SkillDigest> {
        let paths = [Some(self.skill_path), self.old_skill_path];
        let mut digests = HashMap::new();
        for path in paths.into_iter().flatten() {
            match compute_skill_digest(path) {
                Ok(digest) => {
                    digests.insert(path.to_path_buf(), digest);
                }
                Err(e) => eprintln!("failed to hash skill '{}' before the pass: {}", path.display(), e),
            }
        }
        digests
    }

    /// Record what the pass did to the skill directories it handed out.
    ///
    /// A run that rewrote the skill rewrote it for every lane that was still reading it,
    /// so the finding belongs on all of them, and the warning says as much rather than
    /// letting a reader charge it to whichever run happened to finish next.
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    fn record_pass_integrity(&self, runs: &mut [RunRecord], baseline: &HashMap<PathBuf, SkillDigest>) {
        let mut tampered: HashMap<&PathBuf, Vec<String>> = HashMap::new();
        let mut unreadable: HashMap<&PathBuf, String> = HashMap::new();
        for (path, before) in baseline {
            match compute_skill_digest(path) {
                Ok(after) => {
                    let files = detect_tampering(before, &after);
                    if !files.is_empty() {
                        tampered.insert(path, files);
                    }
                }
                Err(e) => {
                    eprintln!("failed to hash skill '{}' after the pass: {}", path.display(), e);
                    unreadable.insert(path, e.to_string());
                }
            }
        }

        for run in runs.iter_mut() {
            if run.cache.is_some() || run.status == "skipped" {
                continue;
            }
            let Some(source) = self.integrity_source(run.scenario_id).map(Path::to_path_buf) else {
                continue;
            };
            if !baseline.contains_key(&source) {
                continue;
            }
            if let Some(reason) = unreadable.get(&source) {
                run.warnings.push(format!(
                    "the skill directory could not be read after the pass, so what it holds now is unknown: {reason}"
                ));
                run.skill_integrity = Some(SkillIntegrityReport::unverifiable());
                continue;
            }
            let files = tampered.get(&source).cloned().unwrap_or_default();
            if !files.is_empty() {
                run.warnings.push(format!(
                    "the skill directory changed during a pass that ran runs side by side, so no single run answers for it: {}",
                    files.join(", ")
                ));
            }
            run.skill_integrity = Some(SkillIntegrityReport::changed(files));
        }
    }

    /// Every reason the harness is not invoked for a run, answered in one place.
    ///
    /// A caller that stops a run early here can only do so by naming a `RunNotStarted`,
    /// which is the same value the report reads back to decide the run is unscoreable.
    /// Deciding it inline instead is how a run refused for an unsupported control ended
    /// up graded against a workspace that was never created.
    fn refuse_to_start(&self, case: &EvalCase) -> Option<RunNotStarted> {
        if let Admission::Exhausted { spent_usd, ceiling_usd } = self.cost_ledger.admit() {
            return Some(RunNotStarted::CostCeilingExhausted { spent_usd, ceiling_usd });
        }

        if case.conversation_history.is_some()
            && !matches!(
                self.runner.support(HarnessControl::ConversationSeeding),
                ControlSupport::Driven(_)
            )
        {
            return Some(RunNotStarted::ControlUnsupported {
                control: HarnessControl::ConversationSeeding,
                runner: self.runner,
            });
        }

        if case.append_system_prompt.is_some()
            && !matches!(
                self.runner.support(HarnessControl::SystemPromptAppend),
                ControlSupport::Driven(_)
            )
        {
            return Some(RunNotStarted::ControlUnsupported {
                control: HarnessControl::SystemPromptAppend,
                runner: self.runner,
            });
        }

        if let Some(grant) = self.resolved_tool_grant(case) {
            if !matches!(
                self.runner.support(HarnessControl::ToolAllowlist),
                ControlSupport::Driven(_)
            ) {
                return Some(RunNotStarted::ControlUnsupported {
                    control: HarnessControl::ToolAllowlist,
                    runner: self.runner,
                });
            }
            if grant.is_empty() {
                return Some(RunNotStarted::ToolGrantUnrepresentable { runner: self.runner });
            }
        }

        None
    }

    /// The tool grant this run is actually given, once the operator's ceiling and the
    /// case's own declaration are combined.
    fn resolved_tool_grant(&self, case: &EvalCase) -> Option<ToolGrant> {
        effective_tool_grant(self.allowed_tools.as_ref(), case.allowed_tools.as_ref())
    }

    fn execute(&self, run: &mut RunRecord, integrity: IntegrityWindow, lane: Lane) {
        let span = tracing::info_span!(
            "run",
            "otel.status_code" = Empty,
            { EVAL_RUN_ID } = run.id.as_str(),
            { EVAL_CASE_ID } = run.eval_case_id.as_str(),
            { EVAL_CASE_SPLIT } = run.split.as_str(),
            { EVAL_SCENARIO } = run.scenario_id.as_str(),
            { EVAL_ITERATION } = i64::from(run.iteration),
            { EVAL_MODEL_CONFIG } = run.model_config_id.as_str(),
            { EVAL_RUNNER_KIND } = self.runner_kind.as_str(),
            { EVAL_RUNNER_VERSION } = self.runner_version.as_deref(),
            { EVAL_RUNNER_MODEL } = Empty,
            { EVAL_TOOL_GRANT } = Empty,
            { EVAL_LANE } = i64::try_from(lane.0).unwrap_or(i64::MAX),
            { EVAL_CACHE_HIT } = Empty,
            { EVAL_RUN_STATUS } = Empty,
            { ERROR_TYPE } = Empty,
        );
        run.trace = SpanReference::of(&span);
        span.in_scope(|| self.execute_run(run, integrity));

        span.record(EVAL_CACHE_HIT, run.cache.is_some());
        span.record(EVAL_RUN_STATUS, run.status.as_str());
        if let Some(model) = &run.runner_model {
            span.record(EVAL_RUNNER_MODEL, model.as_str());
        }
        if let Some(grant) = &run.tool_grant {
            span.record(EVAL_TOOL_GRANT, grant.to_string().as_str());
        }
        if matches!(run.status.as_str(), "failed" | "timeout" | "budget_exhausted") {
            record_failure(&span, run.failure_kind.as_deref().unwrap_or(RUN_FAILED));
        }
        record_run(run);
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    fn execute_run(&self, run: &mut RunRecord, integrity: IntegrityWindow) {
        let case = match self.case_index.get(&run.eval_case_id) {
            Some(case) => *case,
            None => {
                eprintln!("Skipping run {}: eval case {} not found", run.id, run.eval_case_id);
                return;
            }
        };

        let scenario = run.scenario_id;

        let (integrity_path, old_skill_md_ref, old_skill_path_ref) = match scenario {
            ScenarioKind::OldSkill => {
                let old_path = match self.old_skill_path {
                    Some(path) => path,
                    None => {
                        eprintln!("Run {}: old_skill scenario requires --old-skill-dir", run.id);
                        run.status = "failed".to_string();
                        return;
                    }
                };
                let old_md = match self.old_skill_md {
                    Some(md) => md,
                    None => {
                        eprintln!("Run {}: old skill SKILL.md is unavailable", run.id);
                        run.status = "failed".to_string();
                        return;
                    }
                };
                (old_path, Some(old_md), Some(old_path))
            }
            _ => (self.skill_path, None, None),
        };

        let workspace_dir = self.report_dir.join(&run.paths.workspace);
        let run_dir = workspace_dir.parent().unwrap_or(self.report_dir).to_path_buf();
        let transcript_path = run_dir.join("transcript.jsonl");
        let stderr_path = run_dir.join("stderr.log");

        let resolving = step_span("resolve mocks").entered();
        let mock_set = match resolve_mock_set(self.skill_path, &self.eval_dir, &run.eval_case_id) {
            Ok(set) => set,
            Err(e) => {
                record_failure(&resolving, STEP_FAILED);
                eprintln!("Run {}: failed to resolve mcp mocks: {}", run.id, e);
                run.status = "failed".to_string();
                return;
            }
        };

        if !mock_set.is_empty() {
            let support = self.runner.support(HarnessControl::McpServers);
            if !support.is_offered() {
                run.not_started(RunNotStarted::ControlUnsupported {
                    control: HarnessControl::McpServers,
                    runner: self.runner,
                });
                return;
            }
            if support.needs_run_owned_config_home() && !matches!(self.environment, EnvironmentPolicy::Isolated) {
                run.not_started(RunNotStarted::ControlNeedsRunOwnedConfigHome {
                    control: HarnessControl::McpServers,
                    runner: self.runner,
                    environment: self.environment,
                });
                return;
            }
        }
        drop(resolving);

        let hashing = step_span("hash fixtures").entered();
        let fixture_hash = match compute_fixture_hash(self.skill_path, &self.eval_dir, &run.eval_case_id) {
            Ok(hash) => hash.as_str().to_string(),
            Err(e) => {
                record_failure(&hashing, STEP_FAILED);
                eprintln!("Run {}: failed to hash fixtures: {}", run.id, e);
                run.status = "failed".to_string();
                return;
            }
        };
        drop(hashing);

        let keying = step_span("compute cache key").entered();
        let skill_hash = match scenario {
            ScenarioKind::OldSkill => self.old_skill_hash.clone().unwrap_or_else(|| self.skill_hash.clone()),
            _ => self.skill_hash.clone(),
        };

        let scaffold_hash = match self
            .case_index
            .get(&run.eval_case_id)
            .and_then(|case| case.scaffold.as_ref())
        {
            None => None,
            Some(scaffold) => match scaffold.digest(self.skill_path) {
                Ok(digest) => Some(digest.as_str().to_string()),
                Err(e) => {
                    record_failure(&keying, STEP_FAILED);
                    eprintln!("Run {}: {}", run.id, e);
                    run.status = "failed".to_string();
                    return;
                }
            },
        };

        let companion_hash =
            match crate::agentskills::companion_skills::companion_digest(self.skill_path, &case.companion_skills) {
                Ok(digest) => digest.map(|digest| digest.as_str().to_string()),
                Err(e) => {
                    record_failure(&keying, STEP_FAILED);
                    eprintln!("Run {}: failed to hash companion skills: {}", run.id, e);
                    run.status = "failed".to_string();
                    return;
                }
            };

        let resolved_model = effective_model(case, self.runner_model);
        run.runner_model = resolved_model.cloned();
        run.runner_model_source = Some(effective_model_source(case, self.runner_model));

        let tool_grant = self.resolved_tool_grant(case);
        // `ToolGrant` never deserializes empty, and an empty grant always gets refused
        // in `refuse_to_start` below anyway, so the report says why through its warning
        // rather than through a `tool_grant` it could not read back.
        run.tool_grant = tool_grant.clone().filter(|grant| !grant.is_empty());

        let key_input = CacheKeyInput {
            eval_case_id: run.eval_case_id.clone(),
            skill_hash,
            evals_hash: self.evals_hash.clone(),
            fixture_hash,
            mock_hash: if mock_set.is_empty() {
                None
            } else {
                Some(mock_set.content_hash())
            },
            model_config: run.model_config_id.clone(),
            runner_model: resolved_model.map(|model| model.as_str().to_string()),
            runner_kind: self.runner_kind.clone(),
            runner_version: self.runner_version.clone(),
            scenario,
            attempt: run.attempt,
            prompt_contract_version: PROMPT_CONTRACT_VERSION.to_string(),
            environment: self.environment,
            permission: self.permission,
            tool_grant: tool_grant.clone(),
            skill_staging: self.skill_staging,
            scaffold_hash,
            companion_hash,
            caps: self.caps,
        };
        let reuse_input = ReuseKeyInput::of(&key_input);
        let cache_key = CacheKey::from_input(&key_input);
        drop(keying);

        let pointer = {
            let lookup = step_span("look up cache").entered();
            let pointer = try_resolve_cache(self.out_dir, self.cache_options, &key_input, &reuse_input);
            lookup.set_attribute(EVAL_CACHE_HIT, pointer.is_some());
            if self.cache_options.enabled || self.cache_options.reuse_completed {
                record_cache_lookup(pointer.is_some());
            }
            pointer
        };
        if let Some(pointer) = pointer {
            let restoring = step_span("restore from cache").entered();
            match apply_cache_hit(run, &cache_key, &pointer, self.report_dir) {
                Ok(restored) => {
                    restoring.set_attribute(EVAL_CACHE_RESTORED_FILES, restored.files() as i64);
                    restoring.set_attribute(EVAL_CACHE_RESTORED_BYTES, restored.bytes() as i64);
                    return;
                }
                Err(e) => {
                    record_failure(&restoring, STEP_FAILED);
                    eprintln!("Run {}: cache reuse failed, re-executing: {}", run.id, e);
                }
            }
        }

        if let Some(reason) = step_span("admit budget").in_scope(|| self.refuse_to_start(case)) {
            run.not_started(reason);
            return;
        }

        let mcp_config = if mock_set.is_empty() {
            None
        } else {
            let materializing = step_span("materialize mocks").entered();
            match materialize_mock_set_for_run(&mock_set, &run_dir, &MockServerEnv::none()) {
                Ok(config) => Some(config),
                Err(e) => {
                    record_failure(&materializing, STEP_FAILED);
                    eprintln!("Run {}: failed to materialize mcp mocks: {}", run.id, e);
                    run.status = "failed".to_string();
                    return;
                }
            }
        };

        let request = EvalRunRequest {
            eval: case,
            scenario,
            skill_md: self.skill_md,
            skill_path: self.skill_path,
            old_skill_md: old_skill_md_ref,
            old_skill_path: old_skill_path_ref,
            workspace_dir: &workspace_dir,
            transcript_path: &transcript_path,
            stderr_path: &stderr_path,
            runner_model: resolved_model.map(ModelName::as_str),
            timeout_secs: effective_timeout_secs(case, self.timeout_secs),
            skill_staging: self.skill_staging,
            environment: self.environment,
            permission: self.permission,
            tool_grant,
            scaffold_permission: self.scaffold_permission,
            eval_dir: self.eval_dir.clone(),
            mcp_config,
            telemetry_forwarding: self.telemetry_forwarding,
            caps: self.caps,
        };

        let digest_before = match integrity {
            IntegrityWindow::Pass => None,
            IntegrityWindow::Run => step_span("hash skill").in_scope(|| match compute_skill_digest(integrity_path) {
                Ok(digest) => Some(digest),
                Err(e) => {
                    record_failure(&tracing::Span::current(), STEP_FAILED);
                    eprintln!("Run {}: failed to hash skill before invoke: {}", run.id, e);
                    None
                }
            }),
        };

        let max_attempts = self.retries.saturating_add(1);
        let mut invocations = 0u32;
        let mut final_outcome = None;

        for _ in 0..max_attempts {
            invocations += 1;
            let attempt = tracing::info_span!(
                "attempt",
                "otel.status_code" = Empty,
                { EVAL_ATTEMPT } = i64::from(invocations),
                { EVAL_ATTEMPT_OUTCOME } = Empty,
                { EVAL_ATTEMPT_TRANSIENT } = Empty,
                { ERROR_TYPE } = Empty,
            );
            let result = attempt.in_scope(|| {
                discard_mock_calls_from_earlier_attempts(&run_dir, &run.id);
                trace_mock_servers(&mock_set, &run_dir, self.telemetry_forwarding);
                let result = invoke_runner(self.runner, &request);
                if let Ok(outcome) = &result {
                    self.cost_ledger.record(outcome.cost.as_ref());
                }
                result
            });
            match result {
                Ok(outcome) => {
                    let ended = if outcome.is_transient_failure() {
                        AttemptOutcome::TransientFailure
                    } else {
                        AttemptOutcome::Completed
                    };
                    finish_attempt(&attempt, ended);
                    final_outcome = Some(outcome);
                    if !ended.is_transient() {
                        break;
                    }
                }
                Err(e) => {
                    finish_attempt(&attempt, AttemptOutcome::RunnerError);
                    eprintln!("Run {} failed: {}", run.id, e);
                    run.status = "failed".to_string();
                    run.failure_kind = Some(crate::agentskills::runner::FAILURE_KIND_RUNNER.to_string());
                    final_outcome = None;
                    break;
                }
            }
        }

        if let Some(outcome) = final_outcome {
            step_span("apply outcome").in_scope(|| {
                apply_outcome(
                    run,
                    case,
                    &outcome,
                    &transcript_path,
                    &stderr_path,
                    self.report_dir,
                    &workspace_dir,
                );
                run.metrics.transcript = measure_transcript(self.runner, &transcript_path);
            });
        }
        run.runner_invocations = invocations;

        if self.cache_options.enabled && run.status == "completed" {
            let recording = step_span("record cache entry").entered();
            if let Err(e) = record_completion(self.out_dir, &cache_key, &key_input, self.report_dir, &run.id) {
                record_failure(&recording, STEP_FAILED);
                eprintln!("Run {}: failed to record cache entry: {}", run.id, e);
            }
        }

        if let Some(before) = digest_before {
            let checking = step_span("check skill tampering").entered();
            match compute_skill_digest(integrity_path) {
                Ok(after) => {
                    let tampered = detect_tampering(&before, &after);
                    checking.set_attribute(EVAL_SKILL_TAMPERED, !tampered.is_empty());
                    run.skill_integrity = Some(SkillIntegrityReport::changed(tampered));
                }
                Err(e) => {
                    record_failure(&checking, STEP_FAILED);
                    eprintln!("Run {}: failed to hash skill after invoke: {}", run.id, e);
                    run.warnings.push(format!(
                        "the skill directory could not be read after the run, so what it holds now is unknown: {e}"
                    ));
                    run.skill_integrity = Some(SkillIntegrityReport::unverifiable());
                }
            }
        }
    }
}

/// What a lane has to install before it can run anything.
///
/// Nothing, outside tests. The fake runner lives in thread-local state so tests running
/// side by side cannot see each other's counters, and a lane is a different thread, so it
/// has to be handed the state explicitly or it would reach for a real harness.
#[cfg(not(test))]
struct LaneContext;

#[cfg(test)]
type LaneContext = Option<std::sync::Arc<fake_runner::FakeState>>;

#[cfg(not(test))]
fn lane_context() -> LaneContext {
    LaneContext
}

#[cfg(test)]
fn lane_context() -> LaneContext {
    fake_runner::export()
}

#[cfg(not(test))]
fn adopt_lane_context(_context: LaneContext) {}

#[cfg(test)]
fn adopt_lane_context(context: LaneContext) {
    fake_runner::adopt(context);
}

#[cfg(test)]
mod fake_runner;

/// Materialize a non-empty mock set into `run_dir`, pointing the generated `--mcp-config`
/// at this same `trg` binary's own hidden `mock-server` subcommand.
///
/// `current_exe` is looked up per run rather than once for the whole pass because a pass
/// can run for long enough that re-reading it costs nothing worth caching, and caching it
/// would be one more piece of state a test has to seed.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
fn materialize_mock_set_for_run(
    mock_set: &MockSet,
    run_dir: &Path,
    server_env: &MockServerEnv,
) -> Result<MaterializedMcpConfig, String> {
    let trg_binary = MockServerBinary::locate().map_err(|e| e.to_string())?;
    materialize_mock_set(mock_set, run_dir, &trg_binary, server_env).map_err(|e| e.to_string())
}

/// Rewrite the run's mcp config so the mock servers this attempt starts continue its
/// trace. The config already written stays in place when this fails, since a mock server
/// that starts a trace of its own still answers every call.
fn trace_mock_servers(mock_set: &MockSet, run_dir: &Path, forwarding: TelemetryForwarding) {
    if mock_set.is_empty() {
        return;
    }
    let server_env = MockServerEnv::from_current_trace(forwarding);
    if !server_env.is_empty() {
        let _ = materialize_mock_set_for_run(mock_set, run_dir, &server_env);
    }
}

fn finish_attempt(span: &tracing::Span, outcome: AttemptOutcome) {
    span.record(EVAL_ATTEMPT_OUTCOME, outcome.as_str());
    span.record(EVAL_ATTEMPT_TRANSIENT, outcome.is_transient());
    record_attempt(outcome);
    if outcome != AttemptOutcome::Completed {
        record_failure(span, outcome.as_str());
    }
}

/// Which lane of a concurrent pass executed a run; `0` for a serial pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lane(usize);

impl Lane {
    const SERIAL: Self = Self(0);
}

fn invoke_runner(runner: Runner, request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    #[cfg(test)]
    if fake_runner::enabled() {
        return Ok(fake_runner::next_outcome(request));
    }
    runner.invoke(request)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn apply_outcome(
    run: &mut crate::agentskills::report::RunRecord,
    eval: &EvalCase,
    outcome: &EvalRunOutcome,
    transcript_path: &Path,
    stderr_path: &Path,
    report_dir: &Path,
    workspace_dir: &Path,
) {
    run.status = outcome.status.as_str().to_string();
    run.failure_kind = outcome.failure_kind.map(str::to_string);
    run.metrics.duration_ms = Some(outcome.duration_ms);
    run.metrics.exit_code = outcome.exit_code;
    run.metrics.total_tokens = outcome.tokens.total_tokens();
    run.metrics.input_tokens = outcome.tokens.input_tokens();
    run.metrics.output_tokens = outcome.tokens.output_tokens();
    run.metrics.cached_tokens = outcome.tokens.cached_tokens();
    run.metrics.cost = outcome.cost.clone();
    for field in outcome.tokens.unreadable() {
        run.warnings.push(field.warning());
    }

    run.read_only_fixture_violations = outcome.read_only_fixture_violations.clone();
    for path in &outcome.read_only_fixture_violations {
        run.warnings
            .push(format!("read-only fixture '{path}' was modified during the run"));
    }

    run.artifacts.retain(|artifact| {
        artifact
            .get("kind")
            .and_then(|value| value.as_str())
            .is_none_or(|kind| {
                kind != "transcript"
                    && kind != "stderr"
                    && kind != "runner_command"
                    && kind != "runner_env"
                    && kind != "mock_calls"
            })
    });
    run.mock_violations.clear();

    let transcript_relative = artifact_relative_path(transcript_path, report_dir);
    run.artifacts
        .push(crate::agentskills::report::artifact("transcript", transcript_relative));

    if stderr_path.is_file() {
        let stderr_relative = artifact_relative_path(stderr_path, report_dir);
        run.artifacts
            .push(crate::agentskills::report::artifact("stderr", stderr_relative));
    }

    if let Some(run_dir) = transcript_path.parent() {
        let cmd_path = run_dir.join("cmd");
        if cmd_path.is_file() {
            run.artifacts.push(crate::agentskills::report::artifact(
                "runner_command",
                artifact_relative_path(&cmd_path, report_dir),
            ));
        }
        let env_path = run_dir.join("env.json");
        if env_path.is_file() {
            run.artifacts.push(crate::agentskills::report::artifact(
                "runner_env",
                artifact_relative_path(&env_path, report_dir),
            ));
        }

        let mock_calls_path = run_dir.join(MOCK_CALLS_LOG_NAME);
        if mock_calls_path.is_file() {
            run.artifacts.push(crate::agentskills::report::artifact(
                "mock_calls",
                artifact_relative_path(&mock_calls_path, report_dir),
            ));
            run.mock_violations = read_mock_violations(&mock_calls_path);
        }
    }

    match index_output_artifacts(workspace_dir, report_dir) {
        Ok(outputs) => {
            for artifact in outputs {
                #[derive(serde::Serialize)]
                struct OutputDocument {
                    kind: &'static str,
                    path: String,
                    size_bytes: u64,
                    sha256: String,
                    mime_type: Option<String>,
                }
                run.artifacts.push(
                    serde_json::to_value(OutputDocument {
                        kind: "output",
                        path: artifact.path,
                        size_bytes: artifact.size_bytes,
                        sha256: artifact.sha256,
                        mime_type: artifact.mime_type,
                    })
                    .expect("output document contains strings and integers"),
                );
            }
        }
        Err(e) => {
            eprintln!("Run {}: failed to index output artifacts: {}", run.id, e);
        }
    }

    if outcome.status == crate::agentskills::runner::RunStatus::Completed {
        let outputs_dir = workspace_dir.join(crate::agentskills::outputs::OUTPUTS_DIR);
        run.warnings
            .extend(missing_expected_output_warnings(eval, &outputs_dir));
    }

    if let Ok(transcript) = read_normalized_transcript(transcript_path) {
        if let Some(warning) = workspace_escape_warning(&transcript) {
            eprintln!("Run {}: {}", run.id, warning);
            run.warnings.push(warning);
        }
    }
}

/// Every violation the mock server logged against a call this run made.
///
/// A malformed or unreadable log line is skipped rather than failing the run: the log is
/// a diagnostic aid, and losing one entry from it should never be the reason a run that
/// otherwise completed gets reported as failed.
/// A retried attempt is thrown away whole, so what it asked of the mocks must not be
/// answered for by the attempt that stuck.
///
/// The log is appended to for the life of a run, so left in place the calls of an
/// attempt nobody kept are read back as violations of the run that was kept, and a
/// clean invocation fails for a call it never made.
///
/// Removed rather than emptied: the log exists only once a mock server has loaded its
/// mocks, so an empty one left behind by an attempt whose server did come up would tell
/// the next attempt's reader that a server came up for it too.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn discard_mock_calls_from_earlier_attempts(run_dir: &Path, run_id: &str) {
    let path = run_dir.join(MOCK_CALLS_LOG_NAME);
    if !path.is_file() {
        return;
    }
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!("Run {run_id}: failed to discard the mock call log between attempts: {e}");
    }
}

fn read_mock_violations(mock_calls_path: &Path) -> Vec<crate::agentskills::mocks::MockViolation> {
    let Ok(contents) = std::fs::read_to_string(mock_calls_path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<MockCallLogEntry>(line).ok())
        .flat_map(|entry| entry.violations)
        .collect()
}

/// trg runs each harness's own CLI, so it cannot stop one from reading outside the
/// workspace it was given. Naming what left the workspace is the whole remedy
/// available here, and it belongs in the report rather than only on the terminal.
fn workspace_escape_warning(transcript: &NormalizedTranscript) -> Option<String> {
    const NAMED: usize = 5;

    if !transcript.escaped_workspace() {
        return None;
    }
    let escapes = &transcript.workspace_escapes;
    let named = escapes
        .iter()
        .take(NAMED)
        .map(|escape| format!("{} ({})", escape.path, escape.tool))
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = escapes.len().saturating_sub(NAMED);
    let tail = if remainder > 0 {
        format!(", and {remainder} more")
    } else {
        String::new()
    };
    Some(format!(
        "runner '{}' reached {} path(s) outside the workspace, which trg cannot prevent: {named}{tail}",
        transcript.runner,
        escapes.len()
    ))
}

fn artifact_relative_path(path: &Path, report_dir: &Path) -> String {
    path.strip_prefix(report_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests;
