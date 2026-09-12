use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use crate::agentskills::budget::{Admission, CostCeiling, CostLedger, FAILURE_KIND_BUDGET};
use crate::agentskills::cache::{
    apply_cache_hit, compute_fixture_hash, record_completion, runner_kind_label, try_resolve_cache, CacheKey,
    CacheKeyInput, CacheOptions, ReuseKeyInput, PROMPT_CONTRACT_VERSION,
};
use crate::agentskills::case_selection::CaseSelection;
use crate::agentskills::concurrency::RunConcurrency;
use crate::agentskills::evals::{
    effective_timeout_secs, missing_expected_output_warnings, parse_eval_suite, EvalCase, EvalCheckOptions, EvalSuite,
};
use crate::agentskills::layout::detect_next_iteration;
use crate::agentskills::outputs::index_output_artifacts;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BudgetReport, BuildReportOptions, EnvironmentPolicy, ReportBundle,
    RunRecord, ScenarioKind, SkillIntegrityReport, SkillStaging, WriteReportOptions,
};
use crate::agentskills::runner::{
    availability, compute_skill_digest, detect_tampering, EvalRunOutcome, EvalRunRequest, Runner, RunnerError,
    SkillDigest,
};
use crate::agentskills::sampling::AttemptCount;
use crate::agentskills::workspace_scaffold::ScaffoldPermission;
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;

use super::benchmark::benchmark_report_dir_with_document;
use super::ci_args::EvalCiArgs;
use super::finish_eval_output;
use super::grade::{grade_report_dir_with_report, GradeJsonOutput};
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
        default_values_t = [ScenarioKind::WithSkill],
        help = "Scenario kind to include (repeatable)"
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
        long,
        value_enum,
        value_name = "RUNNER",
        help = "Agent CLI to execute each (eval × scenario). When unset, runs are scaffolded with status: skipped."
    )]
    pub runner: Option<Runner>,

    #[arg(
        long,
        value_name = "MODEL",
        help = "Optional model identifier forwarded to the runner CLI (--model/-m). When unset, the runner CLI picks its own default; CLI-specific string."
    )]
    pub runner_model: Option<String>,

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

    #[arg(
        long,
        value_name = "N",
        default_value_t = AttemptCount::recommended(),
        help = "Draw each (eval case × scenario) this many times; attempt numbers run 1..N within one iteration. More than one draw is what makes a score a measurement rather than a sample of size one"
    )]
    pub attempts: AttemptCount,

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

    #[arg(long, help = "Fail when any eval case has an empty assertions array")]
    pub require_assertions: bool,

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
        default_value_t = EnvironmentPolicy::Scrubbed,
        help = "How much of this machine each run may see: scrubbed (default) replaces the environment with an allowlist; isolated also gives the run its own HOME and harness config home, so installed skills, global instructions, and MCP servers cannot reach it; inherited passes the environment through"
    )]
    pub environment: EnvironmentPolicy,

    #[arg(
        long,
        help = "Run the workspace scaffold a case declares. The script is author-supplied code that runs with your own reach, so a case that declares one fails its runs until this is passed"
    )]
    pub allow_scaffold: bool,

    #[command(flatten)]
    pub ci: EvalCiArgs,
}

impl RunArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let props = match crate::agentskills::validator::validate_skill(fs, &self.skill_dir) {
            Ok(props) => props,
            Err(e) => {
                eprintln!("Skill validation failed: {}", e);
                return 1;
            }
        };

        if let Err(e) = crate::agentskills::evals::check_eval_suite(
            fs,
            &self.skill_dir,
            &props.name,
            EvalCheckOptions {
                require_assertions: self.require_assertions,
                ..EvalCheckOptions::default()
            },
        ) {
            eprintln!("Skill eval validation failed: {}", e);
            return 1;
        }

        if self.lint_evals {
            match crate::agentskills::evals::load_eval_suite(fs, &self.skill_dir) {
                Ok(suite) => {
                    crate::agentskills::evals::print_eval_lint_warnings(
                        &crate::agentskills::evals::lint_eval_suite_fixtures(
                            fs,
                            &self.skill_dir,
                            &suite,
                            crate::agentskills::evals::EvalLintOptions {
                                allow_empty_assertions: self.require_assertions,
                                ..crate::agentskills::evals::EvalLintOptions::default()
                            },
                        ),
                    );
                }
                Err(error) => {
                    eprintln!("Failed to load eval manifest for linting: {error}");
                    return 1;
                }
            }
        }

        if !self.skill_staging.withholds_the_answer_key() {
            eprintln!(
                "Warning: --skill-staging {} stages links into the live skill directory, so a run that follows one reaches the eval suite next to it and can be scored on text it copied. Use the default --skill-staging copy for a score that rules that out.",
                self.skill_staging.as_str()
            );
        }

        let iteration = self
            .iteration
            .unwrap_or_else(|| detect_next_iteration(&self.out_dir, &props.name));

        let distinct_scenarios: HashSet<ScenarioKind> = self.scenario.iter().copied().collect();
        if self.reuse_completed && distinct_scenarios.len() > 1 {
            eprintln!(
                "--reuse-completed cannot be combined with more than one --scenario: a completed run for one scenario is served to the others, so the scenario delta would compare a run against itself. Run one scenario per invocation, or drop --reuse-completed."
            );
            return 1;
        }

        if self.scenario.contains(&ScenarioKind::OldSkill) && self.old_skill_dir.is_none() {
            eprintln!("--old-skill-dir is required when --scenario old_skill is included");
            return 1;
        }

        if let Some(old_skill_dir) = &self.old_skill_dir {
            match crate::agentskills::validator::validate_skill(fs, old_skill_dir) {
                Ok(old_props) => {
                    if old_props.name != props.name && !self.allow_skill_name_mismatch {
                        eprintln!(
                            "Old skill name '{}' does not match current skill name '{}' (pass --allow-skill-name-mismatch to override)",
                            old_props.name, props.name
                        );
                        return 1;
                    }
                }
                Err(e) => {
                    eprintln!("Old skill validation failed: {}", e);
                    return 1;
                }
            }
        }

        let runner_probe = if let Some(runner) = self.runner {
            if cfg!(test) {
                None
            } else {
                match availability::check_runner_available(runner) {
                    Ok(probe) => Some(probe),
                    Err(unavailable) => {
                        availability::eprint_runner_unavailable(&unavailable);
                        return 1;
                    }
                }
            }
        } else {
            None
        };

        let cases = match CaseSelection::parse(&self.cases, &self.tags) {
            Ok(cases) => cases,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
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
            cases,
            ..BuildReportOptions::default()
        };

        let bundle = match build_report_bundle(
            fs,
            &self.skill_dir,
            &self.skill_dir,
            &props.name,
            &self.model_config,
            &self.scenario,
            build_options,
        ) {
            Ok(bundle) => bundle,
            Err(e) => {
                eprintln!("Failed to build eval report bundle: {}", e);
                return 1;
            }
        };

        let report_dir = match write_report_bundle(
            &self.out_dir,
            &bundle,
            WriteReportOptions {
                force: self.force,
                iteration,
            },
        ) {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("Failed to write eval report bundle: {}", e);
                return 1;
            }
        };

        let mut budget_exhausted = false;
        if let Some(runner) = self.runner {
            let cache_options = CacheOptions {
                enabled: !self.no_cache,
                reuse_completed: self.reuse_completed,
            };
            let cost_ledger = CostLedger::new(self.max_cost_usd);
            match execute_runs(
                runner,
                self.runner_model.as_deref(),
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
                ScaffoldPermission::granted(self.allow_scaffold),
                self.concurrency,
                &cost_ledger,
            ) {
                Ok(runs_skipped) => budget_exhausted = runs_skipped > 0 || cost_ledger.overspent(),
                Err(code) => return code,
            }
        }

        let grade_options = GradeOptions {
            grader: GraderMode::Auto,
            grader_provider: crate::agentskills::judge::JudgeProvider::default(),
            grader_model: None,
            grader_command: None,
            strict: false,
        };

        if self.grade {
            let (code, grade_report) = grade_report_dir_with_report(&report_dir, grade_options, self.output_format);
            let last_stage = code != 0 || !self.benchmark;
            if last_stage {
                let exit_code = exit_code_with_budget(code, budget_exhausted);
                if self.output_format.is_json() {
                    return print_json(
                        &GradeJsonOutput::new(&report_dir, exit_code, grade_report.as_ref()),
                        exit_code,
                    );
                }
                return exit_code;
            }
        }

        if self.benchmark {
            let (code, benchmark_doc) = benchmark_report_dir_with_document(&report_dir, BenchmarkOptions::default());
            let exit_code = exit_code_with_budget(code, budget_exhausted);
            if self.output_format.is_json() {
                if let Some(document) = benchmark_doc {
                    return print_json(&BenchmarkJsonOutput::new(&report_dir, exit_code, &document), exit_code);
                }
            }
            return exit_code;
        }

        finish_eval_output(
            &report_dir,
            self.output_format,
            self.ci.policy(),
            &self.ci.thresholds(),
            None,
            budget_exhausted,
        )
    }
}

/// Where a budget stop lands among the codes a stage already reports.
///
/// A stage that failed said so on its own terms, and the budget finding must not swallow
/// that, so 1 still wins over 2. A stage that passed said nothing about spend, so a pass
/// the ceiling cut short cannot report 0 either.
///
/// Reported when the pass got less than it asked for, or paid more than it allowed:
/// either a run was refused, or spend went strictly past the ceiling, which a single
/// run can do on its own because admission is checked rather than reserved. A pass that
/// lands exactly on its ceiling having refused nothing is neither, and exits 0: it did
/// all the work and paid what it said it would.
const EXIT_BUDGET_EXHAUSTED: i32 = 2;

pub(super) fn exit_code_with_budget(code: i32, budget_exhausted: bool) -> i32 {
    if code != 0 {
        code
    } else if budget_exhausted {
        EXIT_BUDGET_EXHAUSTED
    } else {
        0
    }
}

/// The chained-benchmark shape, alongside the chained-grade shape that
/// `grade` and `run --grade` share.
#[derive(serde::Serialize)]
struct BenchmarkJsonOutput<'a> {
    report_dir: String,
    exit_code: i32,
    benchmark: &'a crate::agentskills::benchmark::BenchmarkDocument,
}

impl<'a> BenchmarkJsonOutput<'a> {
    fn new(report_dir: &Path, exit_code: i32, benchmark: &'a crate::agentskills::benchmark::BenchmarkDocument) -> Self {
        Self {
            report_dir: report_dir.display().to_string(),
            exit_code,
            benchmark,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_runs(
    runner: Runner,
    runner_model: Option<&str>,
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
    scaffold_permission: ScaffoldPermission,
    concurrency: RunConcurrency,
    cost_ledger: &CostLedger,
) -> std::result::Result<usize, i32> {
    let skill_md = match std::fs::read_to_string(skill_path.join("SKILL.md")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read SKILL.md: {}", e);
            return Err(1);
        }
    };

    let old_skill_md = if let Some(old_path) = old_skill_path {
        match std::fs::read_to_string(old_path.join("SKILL.md")) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("Failed to read old skill SKILL.md: {}", e);
                return Err(1);
            }
        }
    } else {
        None
    };

    let evals_path = skill_path.join("evals").join("evals.json");
    let suite: EvalSuite = match std::fs::read_to_string(&evals_path)
        .map_err(|e| format!("read {}: {e}", evals_path.display()))
        .and_then(|s| parse_eval_suite(&s).map_err(|e| format!("parse {}: {e}", evals_path.display())))
    {
        Ok(suite) => suite,
        Err(msg) => {
            eprintln!("Failed to load eval suite: {}", msg);
            return Err(1);
        }
    };

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
        scaffold_permission,
        skill_md: &skill_md,
        old_skill_md: old_skill_md.as_deref(),
        case_index: &case_index,
        runner_version,
        runner_kind,
        skill_hash: bundle.document.suite.skill_hash.clone(),
        old_skill_hash: bundle.document.suite.old_skill_hash.clone(),
        evals_hash: bundle.document.suite.evals_hash.clone(),
        cost_ledger,
    };
    execution.execute_all(&mut bundle.document.runs, concurrency);

    rebuild_summaries(&mut bundle);

    let runs_skipped = bundle
        .document
        .runs
        .iter()
        .filter(|run| run.failure_kind.as_deref() == Some(FAILURE_KIND_BUDGET))
        .count();
    bundle.document.budget = Some(BudgetReport {
        ceiling_usd: cost_ledger.ceiling().map(CostCeiling::usd),
        spent_usd: cost_ledger.spent_usd(),
        exhausted: cost_ledger.exhausted(),
        runs_skipped,
    });

    let report_json = match serde_json::to_string_pretty(&bundle.document) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to re-serialize report.json: {}", e);
            return Err(1);
        }
    };
    if let Err(e) = std::fs::write(report_dir.join("report.json"), report_json) {
        eprintln!("Failed to write updated report.json: {}", e);
        return Err(1);
    }

    Ok(runs_skipped)
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
    runner_model: Option<&'a str>,
    timeout_secs: Option<u64>,
    retries: u32,
    skill_path: &'a Path,
    old_skill_path: Option<&'a Path>,
    out_dir: &'a Path,
    report_dir: &'a Path,
    cache_options: CacheOptions,
    skill_staging: SkillStaging,
    environment: EnvironmentPolicy,
    scaffold_permission: ScaffoldPermission,
    skill_md: &'a str,
    old_skill_md: Option<&'a str>,
    case_index: &'a HashMap<String, &'a EvalCase>,
    runner_version: Option<String>,
    runner_kind: String,
    skill_hash: String,
    old_skill_hash: Option<String>,
    evals_hash: String,
    cost_ledger: &'a CostLedger,
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
                self.execute(run, IntegrityWindow::Run);
            }
            return;
        }

        let baseline = self.skill_digests();
        {
            let queue = &Mutex::new(runs.iter_mut().collect::<VecDeque<&mut RunRecord>>());
            thread::scope(|scope| {
                for _ in 0..lanes {
                    let lane_context = lane_context();
                    scope.spawn(move || {
                        adopt_lane_context(lane_context);
                        loop {
                            let next = queue
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .pop_front();
                            match next {
                                Some(run) => self.execute(run, IntegrityWindow::Pass),
                                None => break,
                            }
                        }
                    });
                }
            });
        }
        self.record_pass_integrity(runs, &baseline);
    }

    /// The directory whose contents answer for a scenario's skill.
    fn integrity_source(&self, scenario: ScenarioKind) -> Option<&Path> {
        match scenario {
            ScenarioKind::OldSkill => self.old_skill_path,
            _ => Some(self.skill_path),
        }
    }

    /// Hash every skill directory the pass will hand out, once, before any run starts.
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

    fn execute(&self, run: &mut RunRecord, integrity: IntegrityWindow) {
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

        let fixture_hash = match compute_fixture_hash(self.skill_path, &run.eval_case_id) {
            Ok(hash) => hash.as_str().to_string(),
            Err(e) => {
                eprintln!("Run {}: failed to hash fixtures: {}", run.id, e);
                run.status = "failed".to_string();
                return;
            }
        };

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
                    eprintln!("Run {}: {}", run.id, e);
                    run.status = "failed".to_string();
                    return;
                }
            },
        };

        let key_input = CacheKeyInput {
            eval_case_id: run.eval_case_id.clone(),
            skill_hash,
            evals_hash: self.evals_hash.clone(),
            fixture_hash,
            model_config: run.model_config_id.clone(),
            runner_model: self.runner_model.map(str::to_string),
            runner_kind: self.runner_kind.clone(),
            runner_version: self.runner_version.clone(),
            scenario,
            attempt: run.attempt,
            prompt_contract_version: PROMPT_CONTRACT_VERSION.to_string(),
            environment: self.environment,
            skill_staging: self.skill_staging,
            scaffold_hash,
        };
        let reuse_input = ReuseKeyInput::of(&key_input);
        let cache_key = CacheKey::from_input(&key_input);

        if let Some(pointer) = try_resolve_cache(self.out_dir, self.cache_options, &key_input, &reuse_input) {
            match apply_cache_hit(run, &cache_key, &pointer, self.report_dir) {
                Ok(()) => return,
                Err(e) => {
                    eprintln!("Run {}: cache reuse failed, re-executing: {}", run.id, e);
                }
            }
        }

        if let Admission::Exhausted { spent_usd, ceiling_usd } = self.cost_ledger.admit() {
            run.status = "skipped".to_string();
            run.failure_kind = Some(FAILURE_KIND_BUDGET.to_string());
            run.warnings.push(format!(
                "the pass has spent ${spent_usd:.2} against a ${ceiling_usd:.2} cost ceiling, so this run was not started"
            ));
            return;
        }

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
            runner_model: self.runner_model,
            timeout_secs: effective_timeout_secs(case, self.timeout_secs),
            skill_staging: self.skill_staging,
            environment: self.environment,
            scaffold_permission: self.scaffold_permission,
        };

        let digest_before = match integrity {
            IntegrityWindow::Pass => None,
            IntegrityWindow::Run => match compute_skill_digest(integrity_path) {
                Ok(digest) => Some(digest),
                Err(e) => {
                    eprintln!("Run {}: failed to hash skill before invoke: {}", run.id, e);
                    None
                }
            },
        };

        let max_attempts = self.retries.saturating_add(1);
        let mut invocations = 0u32;
        let mut last_outcome = None;

        for _ in 0..max_attempts {
            invocations += 1;
            match invoke_runner(self.runner, &request) {
                Ok(outcome) => {
                    self.cost_ledger.record(outcome.cost_usd);
                    if !outcome.is_transient_failure() || invocations >= max_attempts {
                        apply_outcome(
                            run,
                            case,
                            &outcome,
                            &transcript_path,
                            &stderr_path,
                            self.report_dir,
                            &workspace_dir,
                        );
                        run.runner_invocations = invocations;
                        last_outcome = None;
                        break;
                    }
                    last_outcome = Some(outcome);
                }
                Err(e) => {
                    eprintln!("Run {} failed: {}", run.id, e);
                    run.status = "failed".to_string();
                    run.failure_kind = Some(crate::agentskills::runner::FAILURE_KIND_RUNNER.to_string());
                    run.runner_invocations = invocations;
                    last_outcome = None;
                    break;
                }
            }
        }

        if let Some(outcome) = last_outcome {
            apply_outcome(
                run,
                case,
                &outcome,
                &transcript_path,
                &stderr_path,
                self.report_dir,
                &workspace_dir,
            );
            run.runner_invocations = invocations;
        }

        if self.cache_options.enabled && run.status == "completed" {
            if let Err(e) = record_completion(self.out_dir, &cache_key, &key_input, self.report_dir, &run.id) {
                eprintln!("Run {}: failed to record cache entry: {}", run.id, e);
            }
        }

        if let Some(before) = digest_before {
            match compute_skill_digest(integrity_path) {
                Ok(after) => {
                    run.skill_integrity = Some(SkillIntegrityReport::changed(detect_tampering(&before, &after)));
                }
                Err(e) => {
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
mod fake_runner {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use crate::agentskills::runner::{EvalRunOutcome, EvalRunRequest, RunStatus};

    /// How long a lane waits for the lanes a test expects alongside it.
    ///
    /// Bounded so a serial implementation fails the expectation instead of hanging on it.
    const LANE_RENDEZVOUS: Duration = Duration::from_secs(2);

    /// The fake runner's state for one test.
    ///
    /// Shared behind an `Arc` rather than held in thread-local cells alone, because a run
    /// executed in a lane is executed on another thread and still has to count against
    /// the same test.
    #[derive(Default)]
    pub struct FakeState {
        enabled: AtomicBool,
        invocations: AtomicUsize,
        last_timeout_secs: Mutex<Option<u64>>,
        lanes: Mutex<LaneCensus>,
        joined: Condvar,
        tamper_case: Mutex<Option<String>>,
        remove_case: Mutex<Option<String>>,
        cost_usd: Mutex<Option<f64>>,
        transient_failures: AtomicUsize,
    }

    #[derive(Default)]
    struct LaneCensus {
        in_flight: usize,
        peak: usize,
        expected: usize,
    }

    thread_local! {
        static STATE: RefCell<Option<Arc<FakeState>>> = const { RefCell::new(None) };
    }

    fn state() -> Arc<FakeState> {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            cell.get_or_insert_with(|| Arc::new(FakeState::default())).clone()
        })
    }

    pub fn enable() {
        state().enabled.store(true, Ordering::SeqCst);
    }

    pub fn disable() {
        state().enabled.store(false, Ordering::SeqCst);
    }

    pub fn reset() {
        let state = state();
        state.invocations.store(0, Ordering::SeqCst);
        *state.last_timeout_secs.lock().expect("fake timeout") = None;
        *state.lanes.lock().expect("fake lanes") = LaneCensus::default();
        *state.tamper_case.lock().expect("fake tamper") = None;
        *state.remove_case.lock().expect("fake remove") = None;
        *state.cost_usd.lock().expect("fake cost") = None;
        state.transient_failures.store(0, Ordering::SeqCst);
    }

    /// Have this case's run rewrite the skill directory it was handed, standing in for an
    /// agent that followed a staged path back out of its workspace.
    pub fn tamper_on_case(case_id: &str) {
        *state().tamper_case.lock().expect("fake tamper") = Some(case_id.to_string());
    }

    /// Have this case's run delete the skill directory it was handed, standing in for an
    /// agent that removed the directory the pass is meant to answer for.
    pub fn remove_skill_on_case(case_id: &str) {
        *state().remove_case.lock().expect("fake remove") = Some(case_id.to_string());
    }

    /// Have every run from here on report this dollar cost, standing in for a runner that
    /// prices what it did.
    pub fn set_cost_usd(cost_usd: f64) {
        *state().cost_usd.lock().expect("fake cost") = Some(cost_usd);
    }

    /// Have the next runs fail the way a flaky harness does, so a test can tell what a
    /// retried run cost in total from what the attempt that finally stuck cost.
    pub fn fail_transiently_times(times: usize) {
        state().transient_failures.store(times, Ordering::SeqCst);
    }

    /// Hold every run until this many are in flight, so a test can tell lanes that
    /// overlapped from runs that merely happened to be fast.
    pub fn expect_lanes(lanes: usize) {
        state().lanes.lock().expect("fake lanes").expected = lanes;
    }

    /// The most runs that were ever in flight at once.
    pub fn peak_lanes() -> usize {
        state().lanes.lock().expect("fake lanes").peak
    }

    pub fn last_timeout_secs() -> Option<u64> {
        *state().last_timeout_secs.lock().expect("fake timeout")
    }

    pub fn enabled() -> bool {
        STATE.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|state| state.enabled.load(Ordering::SeqCst))
        })
    }

    /// The state a lane has to adopt to stand in for the thread that started it.
    pub fn export() -> Option<Arc<FakeState>> {
        STATE.with(|cell| cell.borrow().clone())
    }

    pub fn adopt(state: Option<Arc<FakeState>>) {
        STATE.with(|cell| *cell.borrow_mut() = state);
    }

    pub fn next_outcome(request: &EvalRunRequest) -> EvalRunOutcome {
        let state = state();
        *state.last_timeout_secs.lock().expect("fake timeout") = request.timeout_secs;
        let count = state.invocations.fetch_add(1, Ordering::SeqCst) + 1;
        enter_lane(&state);

        std::fs::create_dir_all(request.workspace_dir).expect("workspace dir");
        let outputs_dir = request.workspace_dir.join(crate::agentskills::outputs::OUTPUTS_DIR);
        if std::fs::create_dir_all(&outputs_dir).is_ok() {
            let _ = std::fs::write(outputs_dir.join("out.json"), r#"{"ok":true}"#);
        }
        std::fs::write(request.transcript_path, format!(r#"{{"invocation":{count}}}"#)).expect("transcript");

        if state
            .tamper_case
            .lock()
            .expect("fake tamper")
            .as_deref()
            .is_some_and(|case| case == request.eval.id.as_str())
        {
            let skill_md = request.skill_path.join("SKILL.md");
            let mut content = std::fs::read_to_string(&skill_md).expect("skill md");
            content.push_str("\nrewritten by the run\n");
            std::fs::write(&skill_md, content).expect("skill md");
        }

        if state
            .remove_case
            .lock()
            .expect("fake remove")
            .as_deref()
            .is_some_and(|case| case == request.eval.id.as_str())
        {
            std::fs::remove_dir_all(request.skill_path).expect("skill dir");
        }

        leave_lane(&state);

        let cost_usd = *state.cost_usd.lock().expect("fake cost");

        let transient = state
            .transient_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok();
        if transient {
            return EvalRunOutcome {
                status: RunStatus::Failed,
                failure_kind: Some(crate::agentskills::runner::FAILURE_KIND_RUNNER),
                duration_ms: count as u64 * 100,
                exit_code: Some(1),
                total_tokens: Some(count as u64),
                input_tokens: Some(count as u64),
                output_tokens: Some(0),
                cost_usd,
                final_text: format!("transient-{count}"),
            };
        }

        EvalRunOutcome {
            status: RunStatus::Completed,
            failure_kind: None,
            duration_ms: count as u64 * 100,
            exit_code: Some(0),
            total_tokens: Some(count as u64),
            input_tokens: Some(count as u64),
            output_tokens: Some(0),
            cost_usd,
            final_text: format!("run-{count}"),
        }
    }

    fn enter_lane(state: &FakeState) {
        let mut census = state.lanes.lock().expect("fake lanes");
        census.in_flight += 1;
        census.peak = census.peak.max(census.in_flight);
        if census.in_flight >= census.expected {
            state.joined.notify_all();
            return;
        }
        let (guard, _) = state
            .joined
            .wait_timeout_while(census, LANE_RENDEZVOUS, |census| census.in_flight < census.expected)
            .expect("fake lanes");
        drop(guard);
    }

    fn leave_lane(state: &FakeState) {
        let mut census = state.lanes.lock().expect("fake lanes");
        census.in_flight -= 1;
    }
}

fn invoke_runner(runner: Runner, request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    #[cfg(test)]
    if fake_runner::enabled() {
        return Ok(fake_runner::next_outcome(request));
    }
    runner.invoke(request)
}

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
    run.metrics.total_tokens = outcome.total_tokens;
    run.metrics.input_tokens = outcome.input_tokens;
    run.metrics.output_tokens = outcome.output_tokens;
    run.metrics.cost_usd = outcome.cost_usd;

    run.artifacts.retain(|artifact| {
        artifact
            .get("kind")
            .and_then(|value| value.as_str())
            .is_none_or(|kind| {
                kind != "transcript" && kind != "stderr" && kind != "runner_command" && kind != "runner_env"
            })
    });

    let transcript_relative = artifact_relative_path(transcript_path, report_dir);
    run.artifacts.push(serde_json::json!({
        "kind": "transcript",
        "path": transcript_relative,
    }));

    if stderr_path.is_file() {
        let stderr_relative = artifact_relative_path(stderr_path, report_dir);
        run.artifacts.push(serde_json::json!({
            "kind": "stderr",
            "path": stderr_relative,
        }));
    }

    if let Some(run_dir) = transcript_path.parent() {
        let cmd_path = run_dir.join("cmd");
        if cmd_path.is_file() {
            run.artifacts.push(serde_json::json!({
                "kind": "runner_command",
                "path": artifact_relative_path(&cmd_path, report_dir),
            }));
        }
        let env_path = run_dir.join("env.json");
        if env_path.is_file() {
            run.artifacts.push(serde_json::json!({
                "kind": "runner_env",
                "path": artifact_relative_path(&env_path, report_dir),
            }));
        }
    }

    match index_output_artifacts(workspace_dir, report_dir) {
        Ok(outputs) => {
            for artifact in outputs {
                run.artifacts.push(serde_json::json!({
                    "kind": "output",
                    "path": artifact.path,
                    "size_bytes": artifact.size_bytes,
                    "sha256": artifact.sha256,
                    "mime_type": artifact.mime_type,
                }));
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

fn rebuild_summaries(bundle: &mut ReportBundle) {
    let mut counts: HashMap<ScenarioKind, (usize, usize, usize, usize)> = HashMap::new();
    for run in &bundle.document.runs {
        let entry = counts.entry(run.scenario_id).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        match run.status.as_str() {
            "completed" => entry.1 += 1,
            "skipped" => entry.2 += 1,
            "failed" | "timeout" => entry.3 += 1,
            _ => {}
        }
    }
    for summary in bundle.document.summaries.by_scenario.iter_mut() {
        if let Some((total, passed, skipped, failed)) = counts.get(&summary.scenario_id) {
            summary.total_runs = *total;
            summary.passed_runs = *passed;
            summary.skipped_runs = *skipped;
            summary.failed_runs = *failed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::redact::redact_transcript_bytes;
    use crate::agentskills::report::ScenarioKind;
    use crate::agentskills::transcript::{
        ToolName, ToolVisibility, TranscriptFormat, WorkspaceBoundary, WorkspaceEscape,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn a_path_reached_outside_the_workspace_becomes_a_run_warning() {
        let mut transcript = NormalizedTranscript::new("cursor-agent", ToolVisibility::Observed, Vec::new());
        assert_eq!(workspace_escape_warning(&transcript), None);

        transcript.workspace_escapes = vec![WorkspaceEscape {
            tool: ToolName::new("read").unwrap(),
            path: "/host/plugins/cache/SKILL.md".to_string(),
        }];

        let warning = workspace_escape_warning(&transcript).unwrap();
        assert!(warning.contains("cursor-agent"), "{warning}");
        assert!(warning.contains("/host/plugins/cache/SKILL.md"), "{warning}");
        assert!(warning.contains("read"), "{warning}");
    }

    #[test]
    fn a_run_that_stayed_in_its_workspace_is_not_warned_about() {
        let workspace = tempfile::tempdir().unwrap();
        let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"outputs/summary.md"}}}}
"#;
        let transcript = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        );
        assert_eq!(workspace_escape_warning(&transcript), None);
    }

    fn write_fixture_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("fixture-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "assertions": ["checks second"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn run_command_builds_expected_bundle_layout() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_fixture_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: Some(1),
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs {
                strict_ci: false,
                fail_on_runner_failure: false,
                fail_on_failed_assertions: false,
                fail_on_missing_grading: false,
                fail_on_pass_rate_regression: false,
                fail_on_token_regression: false,
                fail_on_duration_regression: false,
                min_pass_rate: None,
                max_tokens: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_duration_ms: None,
                baseline: None,
            },
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 0);

        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(report_dirs.len(), 1);

        let report_dir = &report_dirs[0];
        let report_json_path = report_dir.join("report.json");
        assert!(report_json_path.is_file());

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_json_path).unwrap()).unwrap();
        assert_eq!(
            report.get("schema_version").and_then(|value| value.as_str()),
            Some(crate::agentskills::report::SCHEMA_VERSION)
        );
        assert!(report
            .pointer("/suite/skill_hash")
            .and_then(|value| value.as_str())
            .unwrap()
            .starts_with("sha256:"));
        assert!(report
            .pointer("/suite/evals_hash")
            .and_then(|value| value.as_str())
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(report.get("runs").and_then(|value| value.as_array()).unwrap().len(), 4);
        assert_eq!(
            report.pointer("/report/iteration").and_then(|value| value.as_u64()),
            Some(1)
        );
        let first_run = report.get("runs").and_then(|value| value.as_array()).unwrap()[0].clone();
        assert_eq!(first_run.get("eval_slug").and_then(|value| value.as_str()), Some("one"));
        assert_eq!(first_run.get("iteration").and_then(|value| value.as_u64()), Some(1));
        assert_eq!(
            first_run.get("mirror_path").and_then(|value| value.as_str()),
            Some("iteration-1/eval-one/with_skill/")
        );

        let workspace_dir = report_dir.join("runs/run-001/workspace");
        assert!(workspace_dir.is_dir());
        assert!(workspace_dir.join("outputs").is_dir());
        assert_eq!(std::fs::read_dir(&workspace_dir).unwrap().count(), 1);

        let mirror = report_dir.join("iteration-1/eval-one/with_skill");
        assert!(mirror.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&mirror).unwrap(),
            PathBuf::from("../../runs/run-001/workspace")
        );
        assert!(report_dir.join("iteration-1/benchmark.json").is_file());
        assert!(report_dir.join("iteration-1/alias-index.json").is_file());
    }

    fn write_named_skill(root: &Path, dir_name: &str, skill_name: &str) -> PathBuf {
        let skill_dir = root.join(dir_name);
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: fixture\n---\n"),
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            format!(
                r#"{{
                "skill_name": "{skill_name}",
                "evals": [
                    {{
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    }}
                ]
            }}"#
            ),
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn run_command_rejects_old_skill_name_mismatch_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_named_skill(temp.path(), "fixture-skill", "fixture-skill");
        let old_skill_dir = write_named_skill(temp.path(), "other-skill", "other-skill");
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir,
            out_dir,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::OldSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: None,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: Some(old_skill_dir),
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 1);
    }

    #[test]
    fn run_command_allows_old_skill_name_mismatch_when_flag_set() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_named_skill(temp.path(), "fixture-skill", "fixture-skill");
        let old_skill_dir = write_named_skill(temp.path(), "other-skill", "other-skill");
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::OldSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: None,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: Some(old_skill_dir),
            allow_skill_name_mismatch: true,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 0);

        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(report_dirs.len(), 1);

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dirs[0].join("report.json")).unwrap()).unwrap();
        assert!(report.pointer("/suite/old_skill_path").is_some());
        assert!(report.pointer("/suite/old_skill_hash").is_some());
        assert_eq!(
            report
                .pointer("/dimensions/skill_revisions/1/id")
                .and_then(|value| value.as_str()),
            Some("old")
        );
    }

    #[test]
    fn run_command_requires_old_skill_dir_for_old_skill_scenario() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_fixture_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir,
            out_dir,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::OldSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: None,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 1);
    }

    fn write_cacheable_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("cache-skill");
        std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: cache-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "cache-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    }
                ]
            }"#,
        )
        .unwrap();
        std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v1").unwrap();
        skill_dir
    }

    fn run_with_fake_runner(args: RunArgs) -> PathBuf {
        let (status, report_dir) = run_with_fake_runner_reporting_status(args);
        assert_eq!(status, 0, "eval run failed");
        report_dir
    }

    /// Same as `run_with_fake_runner`, without insisting the pass exited clean, so a test
    /// can look at a report a budget or a CI gate stopped from being one.
    fn run_with_fake_runner_reporting_status(args: RunArgs) -> (i32, PathBuf) {
        super::fake_runner::enable();
        let out_dir = args.out_dir.clone();
        let skill_name = std::fs::read_to_string(args.skill_dir.join("SKILL.md"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("name:"))
            .map(str::trim)
            .unwrap()
            .to_string();
        let status = args.handle(&crate::fs::RealFS);
        super::fake_runner::disable();

        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join(skill_name))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let report_dir = report_dirs
            .into_iter()
            .max_by_key(|path| {
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
            .expect("report dir");
        (status, report_dir)
    }

    fn read_report(report_dir: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap()
    }

    fn first_run_duration(report_dir: &Path) -> u64 {
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        report["runs"][0]["metrics"]["duration_ms"]
            .as_u64()
            .expect("duration_ms")
    }

    #[test]
    fn consecutive_runs_with_identical_inputs_hit_cache() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let base = || RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        let first_report = run_with_fake_runner(base());
        assert_eq!(first_run_duration(&first_report), 100);

        let second_report = run_with_fake_runner(base());
        assert_eq!(first_run_duration(&second_report), 100);

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
        let cache = &report["runs"][0]["cache"];
        assert_eq!(cache["hit"], true);
        assert_eq!(cache["source_run_id"], "run-001");
        assert!(cache["key"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn changing_skill_md_invalidates_cache() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let base = || RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        run_with_fake_runner(base());
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: cache-skill\ndescription: changed\n---\n",
        )
        .unwrap();
        let second_report = run_with_fake_runner(base());
        assert_eq!(first_run_duration(&second_report), 200);

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
        assert!(report["runs"][0]["cache"].is_null());
    }

    #[test]
    fn changing_fixture_invalidates_cache() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let base = || RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        run_with_fake_runner(base());
        std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v2").unwrap();
        let second_report = run_with_fake_runner(base());
        assert_eq!(first_run_duration(&second_report), 200);
    }

    #[test]
    fn no_cache_disables_cache_hit() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let base = |no_cache: bool| RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        run_with_fake_runner(base(false));
        let second_report = run_with_fake_runner(base(true));
        assert_eq!(first_run_duration(&second_report), 200);
    }

    /// `--attempts` buys samples, and a sample the cache copied from the draw before it
    /// is not one. A cell is identified by its draw as well, so the second attempt
    /// executes and the second invocation of the same command still reuses both.
    #[test]
    fn each_attempt_of_a_cell_executes_rather_than_copying_the_draw_before_it() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let base = || RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::parse(2).unwrap(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        let first_report = read_report(&run_with_fake_runner(base()));
        assert_eq!(first_report["runs"][0]["metrics"]["duration_ms"], 100);
        assert!(
            first_report["runs"][1]["cache"].is_null(),
            "the second draw must be executed, not copied from the first"
        );
        assert_eq!(
            first_report["runs"][1]["metrics"]["duration_ms"], 200,
            "the second draw is its own run"
        );

        let second_report = read_report(&run_with_fake_runner(base()));
        assert_eq!(second_report["runs"][0]["cache"]["source_run_id"], "run-001");
        assert_eq!(
            second_report["runs"][1]["cache"]["source_run_id"], "run-002",
            "each draw is served the run recorded for that draw"
        );
        assert_eq!(second_report["runs"][1]["metrics"]["duration_ms"], 200);
    }

    /// A pass costs a model call per case, per scenario and per attempt, so the lanes are
    /// the difference between minutes and hours. What they may not change is the report:
    /// a reader compares runs by position across passes, so the order has to come from
    /// the suite rather than from which lane finished first.
    #[test]
    fn lanes_execute_runs_at_once_and_still_report_them_in_suite_order() {
        super::fake_runner::reset();
        super::fake_runner::expect_lanes(2);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::parse(2).unwrap(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }));

        assert_eq!(
            super::fake_runner::peak_lanes(),
            2,
            "both runs have to be in flight at once for the lanes to buy anything"
        );
        assert_eq!(report["runs"][0]["eval_case_id"], "one");
        assert_eq!(report["runs"][1]["eval_case_id"], "two");
        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(report["runs"][1]["status"], "completed");
    }

    /// Tamper detection exists to say that the skill a run was scored on is not the skill
    /// it was handed. Lanes read one directory, so a rewrite lands on whichever runs were
    /// still reading it, and charging it to the run whose window happened to contain it
    /// would name a culprit the check cannot identify.
    #[test]
    fn a_skill_rewritten_during_a_concurrent_pass_is_not_charged_to_one_run() {
        super::fake_runner::reset();
        super::fake_runner::expect_lanes(2);
        super::fake_runner::tamper_on_case("one");
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            concurrency: RunConcurrency::parse(2).unwrap(),
            ..base_run_args(&skill_dir, &out_dir)
        }));

        for index in 0..2 {
            let run = &report["runs"][index];
            assert_eq!(
                run["skill_integrity"]["tampered"], true,
                "a rewritten skill was read by every lane still running"
            );
            assert!(
                run["warnings"]
                    .as_array()
                    .expect("warnings")
                    .iter()
                    .any(|warning| warning.as_str().unwrap_or_default().contains("no single run answers")),
                "the report has to say the rewrite cannot be pinned on this run"
            );
        }
    }

    /// One run at a time is the case where the check can name the run, so it still does,
    /// and says nothing about the runs that came before it.
    #[test]
    fn a_skill_rewritten_by_a_serial_run_is_charged_to_that_run() {
        super::fake_runner::reset();
        super::fake_runner::tamper_on_case("two");
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(base_run_args(&skill_dir, &out_dir)));

        assert_eq!(report["runs"][0]["eval_case_id"], "one");
        assert_eq!(
            report["runs"][0]["skill_integrity"]["tampered"], false,
            "the run that ran before the rewrite did not see it"
        );
        assert_eq!(report["runs"][1]["skill_integrity"]["tampered"], true);
        assert!(
            report["runs"][1]["warnings"]
                .as_array()
                .is_none_or(|warnings| warnings.is_empty()),
            "a serial pass can name the run, so there is nothing to qualify"
        );
    }

    /// The check exists to say whether the skill a run was scored on is still the skill it
    /// was handed. A directory that cannot be read back answers neither way, and calling it
    /// unchanged would hide the deletion the check is there to catch.
    #[test]
    fn a_skill_that_cannot_be_read_after_a_concurrent_pass_is_not_reported_as_unchanged() {
        super::fake_runner::reset();
        super::fake_runner::expect_lanes(2);
        super::fake_runner::remove_skill_on_case("two");
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            concurrency: RunConcurrency::parse(2).unwrap(),
            ..base_run_args(&skill_dir, &out_dir)
        }));

        for index in 0..2 {
            let run = &report["runs"][index];
            assert_eq!(
                run["skill_integrity"]["tampered"], true,
                "a skill directory that is gone is not a skill directory that was left alone"
            );
            assert!(
                run["warnings"]
                    .as_array()
                    .expect("warnings")
                    .iter()
                    .any(|warning| warning.as_str().unwrap_or_default().contains("could not be read")),
                "the report has to say why nothing answers for the directory"
            );
        }
    }

    /// One run at a time reads the directory in its own window, and the same rule holds
    /// there: a read that failed is not a comparison that passed.
    #[test]
    fn a_skill_that_cannot_be_read_after_a_serial_run_is_not_reported_as_unchanged() {
        super::fake_runner::reset();
        super::fake_runner::remove_skill_on_case("one");
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(base_run_args(&skill_dir, &out_dir)));

        assert_eq!(report["runs"][0]["eval_case_id"], "one");
        assert_eq!(report["runs"][0]["skill_integrity"]["tampered"], true);
        assert!(
            report["runs"][0]["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning.as_str().unwrap_or_default().contains("could not be read")),
            "the run whose window lost the directory has to say so"
        );
    }

    fn base_run_args(skill_dir: &Path, out_dir: &Path) -> RunArgs {
        RunArgs {
            skill_dir: skill_dir.to_path_buf(),
            out_dir: out_dir.to_path_buf(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            allow_scaffold: false,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
    }

    /// The default decides what almost every pass measures, and a pass that draws each
    /// cell once reports a number with no spread attached: any delta it prints against
    /// another pass, or against a baseline, is as likely to be the agent's own variance
    /// as it is to be the skill.
    #[test]
    fn a_pass_that_says_nothing_about_attempts_still_samples_each_cell() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from([
            "trg",
            "ai",
            "skills",
            "eval",
            "run",
            "--skill-dir",
            "skill",
            "--out-dir",
            "out",
        ])
        .expect("parses");

        let crate::commands::Commands::Ai {
            command: crate::commands::ai::AiCommands::Skills { command },
        } = cli.command
        else {
            panic!("expected an ai skills command");
        };
        let crate::commands::ai::skills::SkillsCommands::Eval(eval) = command else {
            panic!("expected an eval command");
        };
        let super::super::EvalCommands::Run(args) = eval.command else {
            panic!("expected an eval run command");
        };

        assert_eq!(args.attempts, AttemptCount::recommended());
        assert!(
            !args.attempts.is_single(),
            "a default of one draw reports a sample of size one as a measurement"
        );
    }

    /// A cell nobody draws is a row the report cannot fill in, so the count is refused at
    /// the boundary rather than quietly raised to one later.
    #[test]
    fn an_attempt_count_of_zero_is_refused_at_the_command_line() {
        use clap::Parser;

        assert!(crate::cli::Cli::try_parse_from([
            "trg",
            "ai",
            "skills",
            "eval",
            "run",
            "--skill-dir",
            "skill",
            "--out-dir",
            "out",
            "--attempts",
            "0",
        ])
        .is_err());
    }

    fn write_two_case_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("lane-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: lane-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "lane-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "assertions": ["checks second"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn reuse_completed_serves_the_same_arm_produced_under_another_model_config() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        run_with_fake_runner(RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let second_report = run_with_fake_runner(RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "other-model".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: Some("gpt-test".to_string()),
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: true,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
        let cache = &report["runs"][0]["cache"];
        assert_eq!(cache["hit"], true);
        assert_eq!(cache["source_run_id"], "run-001");
        assert_eq!(first_run_duration(&second_report), 100);
    }

    /// The two arms of a case differ in the scenario and nothing else, so answering the
    /// baseline with the with-skill run would compare a run against itself.
    #[test]
    fn reuse_completed_does_not_serve_the_with_skill_run_to_the_baseline() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        run_with_fake_runner(RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let second_report = run_with_fake_runner(RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "other-model".to_string(),
            scenario: vec![ScenarioKind::WithoutSkill],
            runner: Some(Runner::Codex),
            runner_model: Some("gpt-test".to_string()),
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: true,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
        assert!(
            report["runs"][0]["cache"].is_null(),
            "the baseline arm has to be executed rather than answered with the run that had the skill"
        );
    }

    #[test]
    fn reuse_completed_rejects_multiple_scenarios() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_cacheable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir,
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: false,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: true,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 1);
        assert!(!out_dir.exists());
    }

    fn write_timeout_skill(root: &Path, timeout_secs: Option<u32>) -> PathBuf {
        let skill_dir = root.join("timeout-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: timeout-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        let timeout_field = timeout_secs
            .map(|secs| format!(",\n                        \"timeout_secs\": {secs}"))
            .unwrap_or_default();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            format!(
                r#"{{
                "schema_version": 2,
                "skill_name": "timeout-skill",
                "evals": [
                    {{
                        "id": "one",
                        "prompt": "first prompt long enough",
                        "expected_output": "first output long",
                        "assertions": ["checks first"]{timeout_field}
                    }}
                ]
            }}"#
            ),
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn per_eval_timeout_overrides_global_timeout_in_runner_request() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_timeout_skill(temp.path(), Some(42));
        let out_dir = temp.path().join("artifacts");

        run_with_fake_runner(RunArgs {
            skill_dir,
            out_dir,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: Some(99),
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        assert_eq!(super::fake_runner::last_timeout_secs(), Some(42));
    }

    fn write_expected_output_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("expected-output-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: expected-output-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "schema_version": 2,
                "skill_name": "expected-output-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt long enough",
                        "expected_output": "first output long",
                        "assertions": ["checks first"],
                        "expected_output_files": ["report.md", "missing.md"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn missing_expected_output_files_add_run_warnings_without_failing() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_expected_output_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report_dir = run_with_fake_runner(RunArgs {
            skill_dir,
            out_dir,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["runs"][0]["status"], "completed");
        let warnings = report["runs"][0]["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("report.md")));
        assert!(warnings
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("missing.md")));
    }

    fn write_gradable_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("gradable-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: gradable-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "gradable-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "assertions": ["file \"out.json\" exists"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    fn write_two_case_gradable_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("gradable-pair");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: gradable-pair\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "gradable-pair",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "assertions": ["file \"out.json\" exists"]
                    },
                    {
                        "id": "two",
                        "prompt": "create output again",
                        "expected_output": "done",
                        "assertions": ["file \"out.json\" exists"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    fn count_grading_json(report_dir: &Path) -> usize {
        let runs_dir = report_dir.join("runs");
        let Ok(entries) = std::fs::read_dir(&runs_dir) else {
            return 0;
        };
        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join("grading.json").is_file())
            .count()
    }

    fn find_grading_json(report_dir: &Path) -> Option<PathBuf> {
        let runs_dir = report_dir.join("runs");
        if !runs_dir.is_dir() {
            return None;
        }
        for entry in std::fs::read_dir(&runs_dir).ok()? {
            let run_dir = entry.ok()?.path();
            let grading = run_dir.join("grading.json");
            if grading.is_file() {
                return Some(grading);
            }
        }
        None
    }

    #[test]
    fn run_with_grade_and_benchmark_writes_all_artifacts() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_gradable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report_dir = run_with_fake_runner(RunArgs {
            skill_dir,
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: true,
            iteration: Some(1),
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: true,
            benchmark: true,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        assert!(report_dir.join("report.json").is_file());
        assert!(find_grading_json(&report_dir).is_some());
        assert!(report_dir.join("benchmark.json").is_file());
    }

    #[test]
    fn chained_grade_failure_preserves_report_json() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_gradable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report_dir = {
            let status = RunArgs {
                skill_dir: skill_dir.clone(),
                out_dir: out_dir.clone(),
                model_config: "ci-default".to_string(),
                scenario: vec![ScenarioKind::WithSkill],
                runner: None,
                runner_model: None,
                timeout_secs: None,
                retries: 0,
                force: false,
                iteration: Some(1),
                attempts: AttemptCount::single(),
                concurrency: RunConcurrency::serial(),
                old_skill_dir: None,
                allow_skill_name_mismatch: false,
                output_format: OutputFormat::Text,
                grade: false,
                benchmark: false,
                require_assertions: false,
                lint_evals: false,
                no_cache: false,
                reuse_completed: false,
                skill_staging: SkillStaging::Symlink,
                environment: EnvironmentPolicy::Scrubbed,
                allow_scaffold: false,
                cases: Vec::new(),
                tags: Vec::new(),
                max_cost_usd: None,
                ci: EvalCiArgs::default(),
            }
            .handle(&crate::fs::RealFS);
            assert_eq!(status, 0);

            std::fs::read_dir(out_dir.join("gradable-skill"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .next()
                .expect("report dir")
        };

        let report_before = std::fs::read_to_string(report_dir.join("report.json")).expect("report.json");

        let (status, _) = grade_report_dir_with_report(
            &report_dir,
            GradeOptions {
                grader: GraderMode::Script,
                grader_provider: crate::agentskills::judge::JudgeProvider::default(),
                grader_model: None,
                grader_command: Some("/nonexistent/grader-binary".into()),
                strict: false,
            },
            OutputFormat::Text,
        );

        assert_ne!(status, 0);
        assert!(report_dir.is_dir());
        assert_eq!(
            std::fs::read_to_string(report_dir.join("report.json")).unwrap(),
            report_before
        );
    }

    #[test]
    fn run_with_grade_exits_nonzero_when_assertions_fail() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_gradable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            skill_dir,
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: Some(1),
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: true,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);

        assert_ne!(status, 0);
        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("gradable-skill"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(report_dirs.len(), 1);
        assert!(report_dirs[0].join("report.json").is_file());
    }

    #[test]
    fn benchmark_without_grade_flags_missing_grading() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_gradable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report_dir = run_with_fake_runner(RunArgs {
            skill_dir,
            out_dir,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: true,
            iteration: Some(1),
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: true,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        });

        let benchmark: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("benchmark.json")).unwrap()).unwrap();
        assert_eq!(
            benchmark
                .pointer("/scenarios/with_skill/completed/missing_grading")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    /// A pass told to spend nothing, or a ceiling that cannot be compared against spend,
    /// is refused where every other malformed run argument is refused: at the command
    /// line, before a report exists to explain it later.
    #[test]
    fn a_max_cost_that_cannot_bound_a_run_is_refused_at_the_command_line() {
        use clap::Parser;

        for value in ["0", "-5", "nan"] {
            assert!(
                crate::cli::Cli::try_parse_from([
                    "trg",
                    "ai",
                    "skills",
                    "eval",
                    "run",
                    "--skill-dir",
                    "skill",
                    "--out-dir",
                    "out",
                    "--max-cost-usd",
                    value,
                ])
                .is_err(),
                "'{value}' must not parse as a cost ceiling"
            );
        }
    }

    /// A pass that cannot afford everything it was asked to run must say so rather than
    /// pretend it ran everything: the runs the ledger refuses are marked skipped for the
    /// reason the ledger refused them, not dropped or reported as failed runs.
    #[test]
    fn a_pass_under_an_unaffordable_ceiling_skips_the_runs_it_cannot_afford() {
        super::fake_runner::reset();
        super::fake_runner::set_cost_usd(5.0);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
            max_cost_usd: Some(CostCeiling::parse(5.0).unwrap()),
            ..base_run_args(&skill_dir, &out_dir)
        });

        assert_eq!(status, 2, "a budget-exhausted pass has to say so in its exit code");

        let report = read_report(&report_dir);
        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(report["runs"][1]["status"], "skipped");
        assert_eq!(report["runs"][1]["failure_kind"], "budget");
        assert!(
            report["runs"][1]["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning.as_str().unwrap_or_default().contains("cost ceiling")),
            "the report has to say why the run did not start"
        );

        assert_eq!(report["budget"]["ceiling_usd"], 5.0);
        assert_eq!(report["budget"]["spent_usd"], 5.0);
        assert_eq!(report["budget"]["exhausted"], true);
        assert_eq!(report["budget"]["runs_skipped"], 1);
    }

    /// Grading a run that never started reads the empty workspace as a wrong answer, and
    /// every assertion fails. That turns a spending decision into a reported regression,
    /// and the resulting exit 1 hides the exit 2 that would have told an operator to raise
    /// the ceiling rather than go looking for a bug in the skill.
    #[test]
    fn a_run_the_ceiling_refused_is_not_graded_as_a_failure() {
        super::fake_runner::reset();
        super::fake_runner::set_cost_usd(5.0);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_gradable_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
            grade: true,
            max_cost_usd: Some(CostCeiling::parse(5.0).unwrap()),
            ..base_run_args(&skill_dir, &out_dir)
        });

        let report = read_report(&report_dir);
        assert_eq!(report["runs"][1]["status"], "skipped");
        assert_eq!(report["runs"][1]["failure_kind"], "budget");
        assert_eq!(
            count_grading_json(&report_dir),
            1,
            "only the run that produced a workspace has anything to grade"
        );
        assert_eq!(
            status, 2,
            "the pass reports that it ran out of money, not that the skill got something wrong"
        );
    }

    /// Spend reaching the ceiling is not the same as spend passing it. A pass whose last
    /// run lands exactly on the ceiling refused nothing and paid no more than it said it
    /// would, so it has nothing to report that an operator did not already agree to. A
    /// gate that cries wolf on the boundary is a gate a team learns to ignore.
    #[test]
    fn a_pass_that_lands_exactly_on_its_ceiling_without_losing_a_run_still_exits_clean() {
        super::fake_runner::reset();
        super::fake_runner::set_cost_usd(5.0);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
            max_cost_usd: Some(CostCeiling::parse(10.0).unwrap()),
            ..base_run_args(&skill_dir, &out_dir)
        });

        assert_eq!(status, 0, "a pass that did every run inside its ceiling has not failed");

        let report = read_report(&report_dir);
        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(report["runs"][1]["status"], "completed");
        assert_eq!(report["budget"]["spent_usd"], 10.0);
        assert_eq!(report["budget"]["ceiling_usd"], 10.0);
        assert_eq!(
            report["budget"]["runs_skipped"], 0,
            "nothing was refused, so nothing may be reported as skipped"
        );
        assert_eq!(
            report["budget"]["exhausted"], true,
            "the ledger would admit nothing further, and the report still says so"
        );
    }

    /// A pass that prints its result reports the same code it exits with. A reader
    /// parsing the JSON and a shell reading `$?` are asking the same question, and a
    /// budget stop the JSON does not mention is a green result in every dashboard
    /// built on it.
    #[test]
    fn the_exit_code_in_the_json_carries_the_budget_stop_the_process_exits_with() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let (status, report_dir) = run_with_fake_runner_reporting_status(base_run_args(&skill_dir, &out_dir));
        assert_eq!(
            status, 0,
            "the pass itself is clean, so only the budget can move the code"
        );

        let ci = EvalCiArgs::default();
        let stopped = super::super::eval_output(&report_dir, ci.policy(), &ci.thresholds(), None, true)
            .expect("the report is readable");
        assert_eq!(
            stopped.exit_code, 2,
            "a pass the ceiling cut short reports the budget code in the document it prints"
        );

        let ran_fully = super::super::eval_output(&report_dir, ci.policy(), &ci.thresholds(), None, false)
            .expect("the report is readable");
        assert_eq!(
            ran_fully.exit_code, 0,
            "and reports nothing when the ceiling took nothing"
        );
    }

    /// A run the ceiling refused cost money it never spent, once. Retries are invisible in
    /// the report, which keeps only the attempt that stuck, so a ledger fed from the report
    /// prices a flaky pass at a fraction of what it actually billed and lets a ceiling
    /// be walked straight through.
    #[test]
    fn the_ledger_counts_the_attempts_that_were_thrown_away() {
        super::fake_runner::reset();
        super::fake_runner::set_cost_usd(1.0);
        super::fake_runner::fail_transiently_times(2);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let (_status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
            retries: 2,
            ..base_run_args(&skill_dir, &out_dir)
        });

        let report = read_report(&report_dir);
        assert_eq!(
            report["runs"][0]["runner_invocations"], 3,
            "the first case was invoked three times before one stuck"
        );
        assert_eq!(report["runs"][1]["runner_invocations"], 1);
        assert_eq!(
            report["budget"]["spent_usd"], 4.0,
            "every invocation was billed, not only the two the report kept"
        );
    }

    /// A ceiling is checked before a run starts, not before one is served. A cache hit
    /// costs nothing, so a case the pass already paid for in an earlier report has to be
    /// readable from a later report even after that later report has spent its way past
    /// the ceiling on the cases the cache could not answer for.
    #[test]
    fn a_cached_run_is_still_served_after_the_ceiling_is_reached() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = temp.path().join("mixed-skill");
        std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
        std::fs::create_dir_all(skill_dir.join("evals/two")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: mixed-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "mixed-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "assertions": ["checks second"]
                    }
                ]
            }"#,
        )
        .unwrap();
        std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v1").unwrap();
        let out_dir = temp.path().join("artifacts");

        let base = || RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: Some(Runner::Codex),
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            attempts: AttemptCount::single(),
            concurrency: RunConcurrency::serial(),
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            allow_scaffold: false,
            cases: Vec::new(),
            tags: Vec::new(),
            max_cost_usd: None,
            ci: EvalCiArgs::default(),
        };

        // The first pass prices nothing and populates the cache for both cases.
        run_with_fake_runner(base());

        // The second pass changes the fixture case "one" reads, so only "one" has to
        // execute again. It executes at a price the ceiling cannot cover a second run
        // of, while "two" is unchanged and must still come from the cache pass one paid
        // for.
        std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v2").unwrap();
        super::fake_runner::set_cost_usd(5.0);
        let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
            max_cost_usd: Some(CostCeiling::parse(1.0).unwrap()),
            ..base()
        });
        assert_eq!(status, 2, "the second pass has to report the ceiling it hit");

        let report = read_report(&report_dir);
        assert!(
            report["runs"][0]["cache"].is_null(),
            "the fixture change has to force case one to execute again"
        );
        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(
            report["runs"][1]["cache"]["source_run_id"], "run-002",
            "case two is unchanged, so it is still served from the pass that already paid for it"
        );
        assert_eq!(report["runs"][1]["status"], "completed");
        assert_eq!(
            report["budget"]["runs_skipped"], 0,
            "a cache hit is not a run the ledger had to refuse"
        );
    }

    /// A pass told no ceiling has nothing to check a run against, so every case runs
    /// whatever it costs. The ledger still counts what ran, since a number worth reporting
    /// is not the same thing as a number worth enforcing.
    #[test]
    fn a_pass_with_no_ceiling_runs_every_case_even_when_each_one_costs_money() {
        super::fake_runner::reset();
        super::fake_runner::set_cost_usd(50.0);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            max_cost_usd: None,
            ..base_run_args(&skill_dir, &out_dir)
        }));

        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(report["runs"][1]["status"], "completed");
        assert_eq!(report["budget"]["ceiling_usd"], serde_json::Value::Null);
        assert_eq!(report["budget"]["spent_usd"], 100.0);
        assert_eq!(report["budget"]["exhausted"], false);
        assert_eq!(report["budget"]["runs_skipped"], 0);
    }

    /// A runner that never says what a run cost has not run it for free: the ledger must
    /// not treat a missing price as a zero one, or a runner that forgot to price itself
    /// would silently exempt every run it made from the ceiling meant to bound it.
    #[test]
    fn a_runner_that_reports_no_cost_does_not_silently_count_as_free() {
        super::fake_runner::reset();
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            max_cost_usd: Some(CostCeiling::parse(1.0).unwrap()),
            ..base_run_args(&skill_dir, &out_dir)
        }));

        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(
            report["runs"][1]["status"], "completed",
            "an uncosted run must not be mistaken for one the ledger had to refuse"
        );
        assert_eq!(report["budget"]["spent_usd"], 0.0);
        assert_eq!(report["budget"]["exhausted"], false);
        assert_eq!(report["budget"]["runs_skipped"], 0);
    }

    /// The ledger is shared across every lane a pass spawns, so what it reports after a
    /// concurrent pass must equal the sum of what each lane actually spent, whether the
    /// lanes' writes land at the same instant or not.
    #[test]
    fn a_ledger_shared_across_lanes_records_the_sum_of_every_run_it_admitted() {
        super::fake_runner::reset();
        super::fake_runner::expect_lanes(2);
        super::fake_runner::set_cost_usd(3.0);
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_two_case_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let report = read_report(&run_with_fake_runner(RunArgs {
            concurrency: RunConcurrency::parse(2).unwrap(),
            max_cost_usd: Some(CostCeiling::parse(100.0).unwrap()),
            ..base_run_args(&skill_dir, &out_dir)
        }));

        assert_eq!(
            super::fake_runner::peak_lanes(),
            2,
            "both runs have to be in flight at once for this to test the shared ledger"
        );
        assert_eq!(report["runs"][0]["status"], "completed");
        assert_eq!(report["runs"][1]["status"], "completed");
        assert_eq!(
            report["budget"]["spent_usd"], 6.0,
            "two lanes each recording $3 must add to $6, not lose a write to the other"
        );
        assert_eq!(report["budget"]["runs_skipped"], 0);
    }
}
