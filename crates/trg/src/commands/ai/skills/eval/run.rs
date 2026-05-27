use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::agentskills::cache::{
    apply_cache_hit, compute_fixture_hash, record_completion, runner_kind_label, try_resolve_cache, CacheKey,
    CacheKeyInput, CacheOptions, PROMPT_CONTRACT_VERSION, ReuseKeyInput,
};
use crate::agentskills::evals::{
    effective_timeout_secs, missing_expected_output_warnings, parse_eval_suite, EvalCase, EvalCheckOptions,
    EvalSuite,
};
use crate::agentskills::layout::detect_next_iteration;
use crate::agentskills::outputs::index_output_artifacts;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ReportBundle, ScenarioKind, SkillIntegrityReport,
    SkillStaging, WriteReportOptions,
};
use crate::agentskills::runner::{
    availability, compute_skill_digest, detect_tampering, EvalRunOutcome, EvalRunRequest, Runner, RunnerError,
};
use crate::fs::FileSystem;
use clap::Args;

use super::benchmark::benchmark_report_dir_with_document;
use super::ci_args::EvalCiArgs;
use super::finish_eval_output;
use super::grade::grade_report_dir_with_report;
use crate::agentskills::benchmark::BenchmarkOptions;
use crate::agentskills::grading::{GradeOptions, GraderMode};

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
        default_value_t = 1,
        help = "Repeat each (eval case × scenario) this many times; attempt numbers run 1..N within one iteration"
    )]
    pub attempts: u32,

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
        help = "Emit machine-readable JSON for the final pipeline stage to stdout (benchmark if --benchmark, else grade if --grade, else run CI summary). Intermediate stages are not emitted"
    )]
    pub json: bool,

    #[arg(long, help = "Fail when any eval case has an empty assertions array")]
    pub require_assertions: bool,

    #[arg(long, help = "Print eval manifest lint warnings to stderr")]
    pub lint_evals: bool,

    #[arg(long, help = "Disable eval run caching and always execute the runner")]
    pub no_cache: bool,

    #[arg(
        long,
        help = "Reuse any prior completed run for the same eval case, even when scenario or model config differs (still invalidated when skill, evals, or fixtures change)"
    )]
    pub reuse_completed: bool,

    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        default_value_t = SkillStaging::Symlink,
        help = "How to stage the skill into run workspaces: symlink (default) or copy for stricter isolation"
    )]
    pub skill_staging: SkillStaging,

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

        let iteration = self.iteration.unwrap_or_else(|| detect_next_iteration(&self.out_dir, &props.name));

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

        let build_options = BuildReportOptions {
            iteration: Some(iteration),
            attempts: self.attempts.max(1),
            old_skill_path: self.old_skill_dir.clone(),
            user_old_skill_path: self.old_skill_dir.clone(),
            runner: self.runner.map(Runner::display_name).map(str::to_string),
            runner_binary: runner_probe
                .as_ref()
                .map(|probe| probe.binary_path.to_string_lossy().into_owned()),
            runner_version: runner_probe.as_ref().and_then(|probe| probe.version.clone()),
            skill_staging: self.skill_staging,
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

        if let Some(runner) = self.runner {
            let cache_options = CacheOptions {
                enabled: !self.no_cache,
                reuse_completed: self.reuse_completed,
            };
            if let Err(code) = execute_runs(
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
            ) {
                return code;
            }
        }

        let grade_options = GradeOptions {
            grader: GraderMode::Auto,
            grader_model: None,
            grader_command: None,
            strict: false,
        };

        if self.grade {
            let (code, grade_report) = grade_report_dir_with_report(&report_dir, grade_options);
            if code != 0 {
                if self.json && !self.benchmark {
                    emit_chained_grade_json(&report_dir, code, grade_report.as_ref());
                }
                return code;
            }
            if self.json && !self.benchmark {
                emit_chained_grade_json(&report_dir, code, grade_report.as_ref());
                return code;
            }
        }

        if self.benchmark {
            let (code, benchmark_doc) =
                benchmark_report_dir_with_document(&report_dir, BenchmarkOptions::default());
            if self.json {
                if let Some(document) = benchmark_doc {
                    emit_chained_benchmark_json(&report_dir, code, &document);
                }
            }
            return code;
        }

        finish_eval_output(
            &report_dir,
            self.json,
            self.ci.policy(),
            &self.ci.thresholds(),
            None,
        )
    }
}

fn emit_chained_grade_json(
    report_dir: &Path,
    exit_code: i32,
    grade_report: Option<&crate::agentskills::grading::GradeReport>,
) {
    #[derive(serde::Serialize)]
    struct GradeJsonOutput<'a> {
        report_dir: String,
        exit_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        grade: Option<&'a crate::agentskills::grading::GradeReport>,
    }
    let output = GradeJsonOutput {
        report_dir: report_dir.display().to_string(),
        exit_code,
        grade: grade_report,
    };
    match serde_json::to_string_pretty(&output) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("Failed to serialize grade output: {error}"),
    }
}

fn emit_chained_benchmark_json(
    report_dir: &Path,
    exit_code: i32,
    document: &crate::agentskills::benchmark::BenchmarkDocument,
) {
    #[derive(serde::Serialize)]
    struct BenchmarkJsonOutput<'a> {
        report_dir: String,
        exit_code: i32,
        benchmark: &'a crate::agentskills::benchmark::BenchmarkDocument,
    }
    let output = BenchmarkJsonOutput {
        report_dir: report_dir.display().to_string(),
        exit_code,
        benchmark: document,
    };
    match serde_json::to_string_pretty(&output) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("Failed to serialize benchmark output: {error}"),
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
) -> std::result::Result<(), i32> {
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

    for run in bundle.document.runs.iter_mut() {
        let case = match case_index.get(&run.eval_case_id) {
            Some(case) => *case,
            None => {
                eprintln!("Skipping run {}: eval case {} not found", run.id, run.eval_case_id);
                continue;
            }
        };

        let scenario = run.scenario_id;

        let (integrity_path, old_skill_md_ref, old_skill_path_ref) = match scenario {
            ScenarioKind::OldSkill => {
                let old_path = match old_skill_path {
                    Some(path) => path,
                    None => {
                        eprintln!("Run {}: old_skill scenario requires --old-skill-dir", run.id);
                        run.status = "failed".to_string();
                        continue;
                    }
                };
                let old_md = match old_skill_md.as_deref() {
                    Some(md) => md,
                    None => {
                        eprintln!("Run {}: old skill SKILL.md is unavailable", run.id);
                        run.status = "failed".to_string();
                        continue;
                    }
                };
                (old_path, Some(old_md), Some(old_path))
            }
            _ => (skill_path, None, None),
        };

        let workspace_dir = report_dir.join(&run.paths.workspace);
        let run_dir = workspace_dir.parent().unwrap_or(report_dir).to_path_buf();
        let transcript_path = run_dir.join("transcript.jsonl");
        let stderr_path = run_dir.join("stderr.log");

        let fixture_hash = match compute_fixture_hash(skill_path, &run.eval_case_id) {
            Ok(hash) => hash.as_str().to_string(),
            Err(e) => {
                eprintln!("Run {}: failed to hash fixtures: {}", run.id, e);
                run.status = "failed".to_string();
                continue;
            }
        };

        let skill_hash = match scenario {
            ScenarioKind::OldSkill => bundle
                .document
                .suite
                .old_skill_hash
                .clone()
                .unwrap_or_else(|| bundle.document.suite.skill_hash.clone()),
            _ => bundle.document.suite.skill_hash.clone(),
        };

        let key_input = CacheKeyInput {
            skill_hash: skill_hash.clone(),
            evals_hash: bundle.document.suite.evals_hash.clone(),
            fixture_hash: fixture_hash.clone(),
            model_config: run.model_config_id.clone(),
            runner_model: runner_model.map(str::to_string),
            runner_kind: runner_kind.clone(),
            runner_version: runner_version.clone(),
            scenario,
            prompt_contract_version: PROMPT_CONTRACT_VERSION.to_string(),
        };
        let reuse_input = ReuseKeyInput {
            eval_case_id: run.eval_case_id.clone(),
            skill_hash,
            evals_hash: bundle.document.suite.evals_hash.clone(),
            fixture_hash,
        };
        let cache_key = CacheKey::from_input(&key_input);

        if let Some(pointer) = try_resolve_cache(out_dir, cache_options, &key_input, &reuse_input) {
            match apply_cache_hit(run, &cache_key, &pointer, report_dir) {
                Ok(()) => continue,
                Err(e) => {
                    eprintln!("Run {}: cache reuse failed, re-executing: {}", run.id, e);
                }
            }
        }

        let request = EvalRunRequest {
            eval: case,
            scenario,
            skill_md: &skill_md,
            skill_path,
            old_skill_md: old_skill_md_ref,
            old_skill_path: old_skill_path_ref,
            workspace_dir: &workspace_dir,
            transcript_path: &transcript_path,
            stderr_path: &stderr_path,
            runner_model,
            timeout_secs: effective_timeout_secs(case, timeout_secs),
            skill_staging,
        };

        let digest_before = match compute_skill_digest(integrity_path) {
            Ok(digest) => Some(digest),
            Err(e) => {
                eprintln!("Run {}: failed to hash skill before invoke: {}", run.id, e);
                None
            }
        };

        let max_attempts = retries.saturating_add(1);
        let mut invocations = 0u32;
        let mut last_outcome = None;

        for _ in 0..max_attempts {
            invocations += 1;
            match invoke_runner(runner, &request) {
                Ok(outcome) => {
                    if !outcome.is_transient_failure() || invocations >= max_attempts {
                        apply_outcome(
                            run,
                            case,
                            &outcome,
                            &transcript_path,
                            &stderr_path,
                            report_dir,
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
                report_dir,
                &workspace_dir,
            );
            run.runner_invocations = invocations;
        }

        if cache_options.enabled && run.status == "completed" {
            if let Err(e) = record_completion(
                out_dir,
                &cache_key,
                &key_input,
                &run.eval_case_id,
                report_dir,
                &run.id,
            ) {
                eprintln!("Run {}: failed to record cache entry: {}", run.id, e);
            }
        }

        if let Some(before) = digest_before {
            match compute_skill_digest(integrity_path) {
                Ok(after) => {
                    let tampered_files = detect_tampering(&before, &after);
                    run.skill_integrity = Some(SkillIntegrityReport {
                        tampered: !tampered_files.is_empty(),
                        tampered_files,
                    });
                }
                Err(e) => {
                    eprintln!("Run {}: failed to hash skill after invoke: {}", run.id, e);
                }
            }
        }
    }

    rebuild_summaries(&mut bundle);

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

    Ok(())
}

#[cfg(test)]
mod fake_runner {
    use std::cell::Cell;

    use crate::agentskills::runner::{EvalRunOutcome, EvalRunRequest, RunStatus};

    thread_local! {
        static ENABLED: Cell<bool> = const { Cell::new(false) };
        static INVOCATIONS: Cell<usize> = const { Cell::new(0) };
        static LAST_TIMEOUT_SECS: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub fn enable() {
        ENABLED.with(|enabled| enabled.set(true));
    }

    pub fn disable() {
        ENABLED.with(|enabled| enabled.set(false));
    }

    pub fn reset() {
        INVOCATIONS.with(|count| count.set(0));
        LAST_TIMEOUT_SECS.with(|timeout| timeout.set(None));
    }

    pub fn last_timeout_secs() -> Option<u64> {
        LAST_TIMEOUT_SECS.with(|timeout| timeout.get())
    }

    pub fn enabled() -> bool {
        ENABLED.with(|enabled| enabled.get())
    }

    pub fn next_outcome(request: &EvalRunRequest) -> EvalRunOutcome {
        LAST_TIMEOUT_SECS.with(|timeout| timeout.set(request.timeout_secs));
        let count = INVOCATIONS.with(|counter| {
            let next = counter.get() + 1;
            counter.set(next);
            next
        });
        std::fs::create_dir_all(request.workspace_dir).expect("workspace dir");
        let outputs_dir = request.workspace_dir.join(crate::agentskills::outputs::OUTPUTS_DIR);
        if std::fs::create_dir_all(&outputs_dir).is_ok() {
            let _ = std::fs::write(outputs_dir.join("out.json"), r#"{"ok":true}"#);
        }
        std::fs::write(
            request.transcript_path,
            format!(r#"{{"invocation":{count}}}"#),
        )
        .expect("transcript");

        EvalRunOutcome {
            status: RunStatus::Completed,
            failure_kind: None,
            duration_ms: count as u64 * 100,
            exit_code: Some(0),
            total_tokens: Some(count as u64),
            input_tokens: Some(count as u64),
            output_tokens: Some(0),
            cost_usd: None,
            final_text: format!("run-{count}"),
        }
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
                kind != "transcript"
                    && kind != "stderr"
                    && kind != "runner_command"
                    && kind != "runner_env"
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
        run.warnings.extend(missing_expected_output_warnings(eval, &outputs_dir));
    }
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
    use crate::agentskills::report::ScenarioKind;
    use std::path::{Path, PathBuf};

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
            attempts: 1,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,

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
            attempts: 1,
            old_skill_dir: Some(old_skill_dir),
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            old_skill_dir: Some(old_skill_dir),
            allow_skill_name_mismatch: true,
            json: false,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,

            require_assertions: false,
            lint_evals: false,

            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
        assert_eq!(status, 0, "eval run failed");

        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join(skill_name))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        report_dirs
            .into_iter()
            .max_by_key(|path| {
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
            .expect("report dir")
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            ci: EvalCiArgs::default(),
        };

        run_with_fake_runner(base(false));
        let second_report = run_with_fake_runner(base(true));
        assert_eq!(first_run_duration(&second_report), 200);
    }

    #[test]
    fn reuse_completed_reuses_across_scenarios() {
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: true,
            skill_staging: SkillStaging::Symlink,
            ci: EvalCiArgs::default(),
        });

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
        let cache = &report["runs"][0]["cache"];
        assert_eq!(cache["hit"], true);
        assert_eq!(cache["source_run_id"], "run-001");
        assert_eq!(first_run_duration(&second_report), 100);
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            force: true,
            iteration: None,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            ci: EvalCiArgs::default(),
        });

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["runs"][0]["status"], "completed");
        let warnings = report["runs"][0]["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|warning| warning.as_str().unwrap().contains("report.md")));
        assert!(warnings.iter().any(|warning| warning.as_str().unwrap().contains("missing.md")));
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
            attempts: 1,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: true,
            benchmark: true,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
                attempts: 1,
                old_skill_dir: None,
                allow_skill_name_mismatch: false,
                json: false,
                grade: false,
                benchmark: false,
                require_assertions: false,
                lint_evals: false,
                no_cache: false,
                reuse_completed: false,
                skill_staging: SkillStaging::Symlink,
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

        let report_before =
            std::fs::read_to_string(report_dir.join("report.json")).expect("report.json");

        let status = crate::commands::ai::skills::eval::grade::grade_report_dir(
            &report_dir,
            GradeOptions {
                grader: GraderMode::Script,
                grader_model: None,
                grader_command: Some("/nonexistent/grader-binary".into()),
                strict: false,
            },
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
            attempts: 1,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: true,
            benchmark: false,
            require_assertions: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
            attempts: 1,
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            json: false,
            grade: false,
            benchmark: true,
            require_assertions: false,
            lint_evals: false,
            no_cache: true,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
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
}
