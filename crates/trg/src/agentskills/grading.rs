//! Mechanical and script-based grading for skill eval runs.
//!
//! # Mechanical assertion patterns
//!
//! Assertions are matched case-insensitively against the following phrasing:
//!
//! | Kind | Example patterns |
//! |------|------------------|
//! | File exists | `file "out.json" exists`, `outputs/report.md exists` |
//! | File count | `file count is 3`, `contains 2 files`, `3 files in outputs` |
//! | Valid JSON | `valid json`, `out.json is valid json` |
//! | Valid CSV | `valid csv`, `data.csv is valid csv` |
//! | Markdown headings | `valid markdown headings`, `report.md has valid markdown headings` |
//! | Image exists | `image chart.png exists`, `image exists at outputs/chart.png` |
//! | Image dimensions | `chart.png is 800x600`, `image dimensions are 800x600` |
//! | Contains string | `contains "hello"`, `output includes summary`, `includes "foo" in out.txt` |
//! | Regex match | `matches regex /pattern/`, `matches /foo.*/` |
//! | Row count | `row count is 10`, `data.csv has 10 rows`, `10 rows in data.csv` |
//! | Schema validation | `validates against schema foo`, `schema validation for out.json` |

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::evals::{EvalCase, EvalError, EvalSuite, Result};
use super::graders::{self, CaseGrader, GradeInput, Grader, GraderOutcome};
use super::judge::{self, JudgeEndpoint, JudgeModel, JudgeProvider, JudgeRequest};
use super::judge_votes::{tally_opinions, JudgeVoteTally, JudgeVotes};
use super::outputs::FINAL_MD;
use super::report::{ReportDocument, RunRecord};
use super::transcript::{read_normalized_transcript, NormalizedTranscript};
use super::validation::{ValidationError, ValidationErrors};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraderKind {
    Mechanical,
    Declarative,
    Llm,
    Script,
    NeedsLlm,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GraderMode {
    #[default]
    Auto,
    None,
    Llm,
    Script,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct GraderInfo {
    pub kind: GraderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct AssertionGradeResult {
    #[serde(alias = "text")]
    #[schemars(length(min = 1))]
    pub assertion: String,
    pub passed: bool,
    pub evidence: String,
    pub grader: GraderInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Present when the property cannot be observed on the runner that produced
    /// the run. Such a result is neither a pass nor a failure, so it is excluded
    /// from `summary.pass_rate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<String>,
    /// Present when the grader presupposes the skill, so its `passed` value is
    /// an indicator to read rather than a score to count. Kept out of
    /// `summary.pass_rate` in both arms, so the gap between them measures the
    /// work and not the premise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
    /// No grader could answer this assertion at all: the text matched no mechanical
    /// pattern and no judge was consulted. Distinct from `unsupported`, which means a
    /// grader existed and the harness could not answer it, and from `excluded`, which
    /// means the author scoped it to the other arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ungraded: Option<String>,
    /// How a panel of judges split, present only when more than one opinion was taken.
    /// A single opinion has no split to report, and `passed` already carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<JudgeVoteTally>,
}

/// Which single bucket a result belongs to.
///
/// The three markers are independent options, so one result can carry several
/// at once and anything counting over them has to decide which wins. Deciding
/// that here, once, is what stops a printed list from naming assertions that
/// the number printed beside it does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionOutcome {
    /// Deliberately out of scope for this arm, which settles the result no
    /// matter what else is true of it: a check nobody was going to score loses
    /// nothing by also having been unanswerable.
    Excluded,
    /// The harness cannot answer this kind of question at all.
    Unsupported,
    /// In scope and answerable in principle, but nothing attempted it.
    Ungraded,
    /// Counted for or against the skill.
    Scored,
}

impl AssertionGradeResult {
    pub fn is_unsupported(&self) -> bool {
        self.unsupported.is_some()
    }

    pub fn is_excluded(&self) -> bool {
        self.excluded.is_some()
    }

    pub fn is_ungraded(&self) -> bool {
        self.ungraded.is_some()
    }

    pub fn outcome(&self) -> AssertionOutcome {
        if self.is_excluded() {
            AssertionOutcome::Excluded
        } else if self.is_unsupported() {
            AssertionOutcome::Unsupported
        } else if self.is_ungraded() {
            AssertionOutcome::Ungraded
        } else {
            AssertionOutcome::Scored
        }
    }

    pub fn is_scored(&self) -> bool {
        self.outcome() == AssertionOutcome::Scored
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    #[serde(default)]
    pub unsupported: usize,
    #[serde(default)]
    pub excluded: usize,
    /// No grader could even attempt these, so they carry no signal either way.
    #[serde(default)]
    pub ungraded: usize,
    /// Over the scored assertions only, so an ungradable property cannot drag a
    /// skill's score down on a runner that simply cannot be observed. `null` when
    /// nothing was scored at all, because no assertion answered is not the same
    /// result as every assertion failed.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub pass_rate: Option<f64>,
}

/// The assertions the ungraded count counted.
///
/// The number and the list beneath it are two views of one set, so they are
/// taken with one predicate rather than two that have to be kept in step.
fn ungraded_assertion_texts(results: &[AssertionGradeResult]) -> Vec<String> {
    results
        .iter()
        .filter(|result| result.outcome() == AssertionOutcome::Ungraded)
        .map(|result| result.assertion.clone())
        .collect()
}

/// The single place the grading arithmetic lives, so the writer and both
/// validators cannot drift from one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradingCounts {
    pub passed: usize,
    pub failed: usize,
    pub unsupported: usize,
    pub excluded: usize,
    pub ungraded: usize,
    pub total: usize,
}

impl GradingCounts {
    pub fn tally(results: &[AssertionGradeResult]) -> Self {
        let mut passed = 0;
        let mut unsupported = 0;
        let mut excluded = 0;
        let mut ungraded = 0;
        for result in results {
            match result.outcome() {
                AssertionOutcome::Excluded => excluded += 1,
                AssertionOutcome::Unsupported => unsupported += 1,
                AssertionOutcome::Ungraded => ungraded += 1,
                AssertionOutcome::Scored if result.passed => passed += 1,
                AssertionOutcome::Scored => {}
            }
        }
        let total = results.len();
        Self {
            passed,
            failed: total - passed - unsupported - excluded - ungraded,
            unsupported,
            excluded,
            ungraded,
            total,
        }
    }

    pub fn scored(self) -> usize {
        self.total - self.unsupported - self.excluded - self.ungraded
    }

    pub fn pass_rate(self) -> Option<f64> {
        if self.scored() == 0 {
            None
        } else {
            Some(self.passed as f64 / self.scored() as f64)
        }
    }

    pub fn summary(self) -> GradingSummary {
        GradingSummary {
            passed: self.passed,
            failed: self.failed,
            total: self.total,
            unsupported: self.unsupported,
            excluded: self.excluded,
            ungraded: self.ungraded,
            pass_rate: self.pass_rate(),
        }
    }
}

pub fn pass_rate_matches(reported: Option<f64>, expected: Option<f64>) -> bool {
    match (reported, expected) {
        (None, None) => true,
        (Some(reported), Some(expected)) => (reported - expected).abs() <= 0.0001,
        _ => false,
    }
}

pub fn describe_pass_rate(rate: Option<f64>) -> String {
    match rate {
        Some(rate) => format!("{rate}"),
        None => "not scored".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingFile {
    pub assertion_results: Vec<AssertionGradeResult>,
    pub summary: GradingSummary,
}

#[derive(Debug, Clone, Default)]
pub struct GradeOptions {
    pub grader: GraderMode,
    pub grader_provider: JudgeProvider,
    pub grader_model: Option<String>,
    pub grader_command: Option<String>,
    pub grader_votes: JudgeVotes,
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GradeReport {
    pub runs_graded: usize,
    pub assertions_graded: usize,
    pub passed: usize,
    pub failed: usize,
    /// Every assertion no grader could even attempt, mechanical or judge. Sourced
    /// from the same tally as `unsupported` and `excluded` rather than a separate
    /// count, so this can never disagree with what the grading files themselves say.
    pub ungraded: usize,
    #[serde(default)]
    pub unsupported: usize,
    #[serde(default)]
    pub excluded: usize,
    #[serde(default)]
    pub ungraded_assertions: Vec<String>,
    #[serde(default)]
    pub run_statuses: GradedRunStatuses,
}

/// The run statuses behind an assertion tally.
///
/// An assertion tally alone cannot separate a skill that failed its checks from
/// a runner that never produced a workspace to check, and both arrive as
/// `0/N passed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GradedRunStatuses {
    pub completed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub skipped: usize,
    pub unrecognized: usize,
}

impl GradedRunStatuses {
    pub fn record(&mut self, status: &str) {
        match status {
            "completed" => self.completed += 1,
            "failed" => self.failed += 1,
            "timeout" => self.timed_out += 1,
            "skipped" => self.skipped += 1,
            _ => self.unrecognized += 1,
        }
    }

    pub fn not_completed(self) -> usize {
        self.failed + self.timed_out + self.skipped + self.unrecognized
    }

    pub fn describe_not_completed(self) -> Option<String> {
        let parts: Vec<String> = [
            (self.failed, "failed"),
            (self.timed_out, "timed out"),
            (self.skipped, "skipped"),
            (self.unrecognized, "of an unrecognized status"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }
}

#[derive(Debug, Clone)]
struct RunContext {
    run_dir: PathBuf,
    workspace_dir: PathBuf,
    outputs_dir: PathBuf,
    transcript_path: PathBuf,
    skill_dir: PathBuf,
}

/// The grading options plus the judge they resolve to, resolved once per
/// bundle so a missing credential is reported before any run is graded rather
/// than part way through one.
#[derive(Debug)]
struct GradeSession<'a> {
    options: &'a GradeOptions,
    judge: Option<JudgeEndpoint>,
}

impl<'a> GradeSession<'a> {
    fn open(options: &'a GradeOptions, suite: &EvalSuite) -> Result<Self> {
        let judge = if suite_needs_a_judge(options, suite) {
            let model = options
                .grader_model
                .as_deref()
                .and_then(JudgeModel::new)
                .ok_or_else(|| {
                    EvalError::Validation(
                        ValidationError::for_field(
                            "--grader-model",
                            "is required to grade with an LLM judge; pass it or drop the llm graders",
                        )
                        .into(),
                    )
                })?;
            Some(JudgeEndpoint::resolve(
                options.grader_provider,
                &model,
                "--grader-model",
            )?)
        } else {
            None
        };

        Ok(Self { options, judge })
    }

    fn endpoint(&self) -> Result<&JudgeEndpoint> {
        self.judge.as_ref().ok_or_else(|| {
            EvalError::Validation(
                ValidationError::for_field("--grader-model", "no LLM judge was resolved for this run").into(),
            )
        })
    }
}

/// Whether anything in the suite can reach the LLM judge under these options.
///
/// `--grader llm` always can. Under `auto` only a declared `llm` grader or a
/// free-text assertion that no mechanical pattern recognizes can, so a suite
/// of typed graders is graded without a credential.
fn suite_needs_a_judge(options: &GradeOptions, suite: &EvalSuite) -> bool {
    match options.grader {
        GraderMode::Llm => true,
        GraderMode::None | GraderMode::Script => false,
        GraderMode::Auto => suite.evals.iter().any(|case| {
            case.graders
                .iter()
                .any(|declared| matches!(declared.grader, Grader::Llm { .. }))
                || case
                    .assertions
                    .iter()
                    .any(|assertion| parse_mechanical_kind(assertion.as_str()).is_none())
        }),
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MechanicalKind {
    FileExists {
        path: String,
    },
    FileCount {
        count: usize,
        dir: Option<String>,
    },
    ValidJson {
        path: Option<String>,
    },
    ValidCsv {
        path: Option<String>,
    },
    ValidMarkdownHeadings {
        path: Option<String>,
    },
    ImageExists {
        path: String,
    },
    ImageDimensions {
        path: String,
        width: u32,
        height: u32,
    },
    ContainsString {
        needle: String,
        path: Option<String>,
    },
    MatchesRegex {
        pattern: String,
        path: Option<String>,
    },
    RowCount {
        count: usize,
        path: Option<String>,
    },
    SchemaValidation {
        schema: Option<String>,
        path: Option<String>,
    },
}

pub fn grade_report_bundle(report_dir: &Path, options: GradeOptions) -> Result<GradeReport> {
    let report_path = report_dir.join("report.json");
    let report_content = std::fs::read_to_string(&report_path).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                format!("report '{}'", report_path.display()),
                format!("failed to read: {e}"),
            )
            .into(),
        )
    })?;
    let mut document: ReportDocument = serde_json::from_str(&report_content)?;

    let skill_path = PathBuf::from(&document.suite.skill_path);
    let suite: EvalSuite = super::case_directories::resolve_eval_suite(&crate::fs::RealFS, &skill_path)?.suite;

    let case_index: HashMap<String, &EvalCase> = suite.evals.iter().map(|c| (c.id.to_string(), c)).collect();

    let session = GradeSession::open(&options, &suite)?;
    let grader_config = build_grader_config(&options);
    if document.dimensions.graders.is_empty() {
        document.dimensions.graders.push(grader_config.clone());
    }

    let mut report = GradeReport {
        runs_graded: 0,
        assertions_graded: 0,
        passed: 0,
        failed: 0,
        ungraded: 0,
        unsupported: 0,
        excluded: 0,
        ungraded_assertions: Vec::new(),
        run_statuses: GradedRunStatuses::default(),
    };

    let runs = document.runs.clone();
    for (run, run_mut) in runs.iter().zip(document.runs.iter_mut()) {
        let case = match case_index.get(&run.eval_case_id) {
            Some(case) => *case,
            None => {
                return Err(EvalError::Validation(
                    ValidationError::for_field(
                        format!("run '{}'", run.id),
                        format!("eval case '{}' not found in evals.json", run.eval_case_id),
                    )
                    .into(),
                ));
            }
        };

        if stopped_by_cost_ceiling(run) {
            report.run_statuses.record(&run.status);
            continue;
        }

        let ctx = run_context(report_dir, run, &skill_path);
        let mut assertion_results = Vec::with_capacity(case.assertions.len() + case.graders.len());

        let declarative = DeclarativeContext::load(&ctx);
        for grader in &case.graders {
            assertion_results.push(grade_declaratively(grader, case, &declarative, &ctx, &session)?);
        }

        for assertion in &case.assertions {
            assertion_results.push(grade_assertion(assertion.as_str(), case, &ctx, &session)?);
        }

        // A read-only fixture that was modified during the run is a failure of the run
        // itself, not a property left for an assertion or grader to notice, so it is added
        // here rather than left to whichever declared checks the case happens to have.
        for path in &run.read_only_fixture_violations {
            assertion_results.push(read_only_fixture_violation_result(path));
        }

        restore_when_nothing_would_be_scored(&mut assertion_results);

        report.assertions_graded += assertion_results.len();
        report
            .ungraded_assertions
            .extend(ungraded_assertion_texts(&assertion_results));
        let counts = GradingCounts::tally(&assertion_results);
        report.passed += counts.passed;
        report.failed += counts.failed;
        report.unsupported += counts.unsupported;
        report.excluded += counts.excluded;
        report.ungraded += counts.ungraded;

        let grading = build_grading_file(assertion_results)?;
        validate_grading_document(&grading, options.strict)?;

        let grading_path = ctx.run_dir.join("grading.json");
        std::fs::write(&grading_path, serde_json::to_string_pretty(&grading)?)?;

        store_grader_artifacts(run_mut, report_dir, &ctx, &options, &grading)?;

        report.run_statuses.record(&run.status);
        report.runs_graded += 1;
    }

    update_report_after_grading(&mut document, &runs, report_dir, &grader_config)?;
    std::fs::write(report_dir.join("report.json"), serde_json::to_string_pretty(&document)?)?;

    Ok(report)
}

fn run_context(report_dir: &Path, run: &RunRecord, skill_path: &Path) -> RunContext {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());
    let outputs_dir = if run_dir.join("outputs").is_dir() {
        run_dir.join("outputs")
    } else {
        workspace_dir.join("outputs")
    };
    let transcript_path = run_dir.join("transcript.jsonl");

    RunContext {
        run_dir,
        workspace_dir,
        outputs_dir,
        transcript_path,
        skill_dir: skill_path.to_path_buf(),
    }
}

fn build_grader_config(options: &GradeOptions) -> serde_json::Value {
    serde_json::json!({
        "mode": format!("{:?}", options.grader).to_lowercase(),
        "provider": options.grader_provider.as_str(),
        "model": options.grader_model,
        "command": options.grader_command,
        "votes": options.grader_votes.count(),
        "strict": options.strict,
    })
}

fn grade_assertion(
    assertion: &str,
    eval_case: &EvalCase,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let options = session.options;
    match options.grader {
        GraderMode::Script => grade_with_script(assertion, eval_case, ctx, options),
        GraderMode::Llm => grade_with_llm(assertion, ctx, session),
        GraderMode::None | GraderMode::Auto => {
            if let Some(kind) = parse_mechanical_kind(assertion) {
                let (passed, evidence) = evaluate_mechanical(&kind, ctx)?;
                Ok(AssertionGradeResult {
                    name: None,
                    assertion: assertion.to_string(),
                    passed,
                    evidence,
                    grader: GraderInfo {
                        kind: GraderKind::Mechanical,
                        model: None,
                        command: None,
                    },
                    rationale: None,
                    unsupported: None,
                    excluded: None,
                    ungraded: None,
                    votes: None,
                })
            } else if options.grader == GraderMode::None {
                Ok(needs_llm_result(
                    assertion,
                    "no mechanical pattern matched and grader mode is none",
                ))
            } else {
                Ok(needs_llm_result(
                    assertion,
                    "assertion requires LLM grading; re-run with --grader llm",
                ))
            }
        }
    }
}

/// The run artifacts a declarative grader reads, loaded once per run.
struct DeclarativeContext {
    final_text: String,
    raw_transcript: String,
    transcript: Option<NormalizedTranscript>,
}

impl DeclarativeContext {
    fn load(ctx: &RunContext) -> Self {
        Self {
            final_text: std::fs::read_to_string(ctx.outputs_dir.join(FINAL_MD)).unwrap_or_default(),
            raw_transcript: std::fs::read_to_string(&ctx.transcript_path).unwrap_or_default(),
            transcript: read_normalized_transcript(&ctx.transcript_path).ok(),
        }
    }

    fn input<'a>(&'a self, ctx: &'a RunContext) -> GradeInput<'a> {
        GradeInput {
            final_text: &self.final_text,
            run_dir: &ctx.run_dir,
            workspace_dir: &ctx.workspace_dir,
            outputs_dir: &ctx.outputs_dir,
            raw_transcript: &self.raw_transcript,
            transcript: self.transcript.as_ref(),
            skill_dir: &ctx.skill_dir,
        }
    }
}

fn grade_declaratively(
    declared: &CaseGrader,
    eval_case: &EvalCase,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let options = session.options;
    let grader = &declared.grader;
    let assertion = grader.describe();
    let outcome = graders::evaluate(grader, &declarative.input(ctx));
    let excluded = declared.exclusion_reason();
    let name = declared.name.as_ref().map(ToString::to_string);

    let declarative_info = GraderInfo {
        kind: GraderKind::Declarative,
        model: None,
        command: None,
    };

    Ok(match outcome {
        GraderOutcome::Passed { evidence } => AssertionGradeResult {
            assertion,
            passed: true,
            evidence,
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: None,
            excluded,
            ungraded: None,
            votes: None,
        },
        GraderOutcome::Failed { evidence } => AssertionGradeResult {
            assertion,
            passed: false,
            evidence,
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: None,
            excluded,
            ungraded: None,
            votes: None,
        },
        GraderOutcome::Unsupported { reason } => AssertionGradeResult {
            assertion,
            passed: false,
            evidence: reason.clone(),
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: Some(reason),
            excluded,
            ungraded: None,
            votes: None,
        },
        GraderOutcome::Deferred { criterion } => {
            let mut result = match options.grader {
                GraderMode::Llm | GraderMode::Auto => grade_with_llm(&criterion, ctx, session)?,
                GraderMode::Script => grade_with_script(&criterion, eval_case, ctx, options)?,
                GraderMode::None => needs_llm_result(&criterion, "grader mode is none, so no judge was consulted"),
            };
            result.excluded = excluded;
            result.name = name;
            result
        }
        GraderOutcome::AuthoringError { reason } => {
            return Err(EvalError::Validation(
                ValidationError::for_field(assertion, reason).into(),
            ));
        }
    })
}

/// Whether the cost ceiling stopped this run before it started.
///
/// Such a run has no workspace and no transcript, so there is nothing for a grader
/// to read. Grading it anyway would read the absence as a wrong answer and fail every
/// assertion, turning a spending decision into a reported regression, and an LLM judge
/// would bill the pass that has already run out of money to do it.
fn stopped_by_cost_ceiling(run: &RunRecord) -> bool {
    run.failure_kind.as_deref() == Some(crate::agentskills::budget::FAILURE_KIND_BUDGET)
}

/// A case whose every check is arm-scoped would otherwise measure nothing at
/// all, which is never what the author meant by writing it, so the exclusions
/// are lifted and the case is scored as declared. But lifting them only helps
/// when doing so would actually produce something scorable; a case that is
/// entirely ungraded, or whose only observable checks are also unsupported,
/// gains nothing from the lift and must not have it applied.
fn restore_when_nothing_would_be_scored(results: &mut [AssertionGradeResult]) {
    let scored = results.iter().filter(|result| result.is_scored()).count();
    if scored > 0 {
        return;
    }
    let lifting_would_score = results
        .iter()
        .any(|result| result.is_excluded() && !result.is_unsupported() && !result.is_ungraded());
    if !lifting_would_score {
        return;
    }
    for result in results.iter_mut() {
        result.excluded = None;
    }
}

fn read_only_fixture_violation_result(path: &str) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: format!("read-only fixture '{path}' is unchanged"),
        passed: false,
        evidence: format!("fixture '{path}' did not match its source after the run"),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
    }
}

fn needs_llm_result(assertion: &str, evidence: &str) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.to_string(),
        grader: GraderInfo {
            kind: GraderKind::NeedsLlm,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: Some(evidence.to_string()),
        votes: None,
    }
}

/// A grader script that did not exit cleanly graded nothing, whatever it printed.
///
/// Recording the crash as a failed assertion reads as evidence the skill did the wrong
/// thing, which is the one thing it is not evidence of. Ungraded is the honest record, and
/// the gate already refuses to pass a suite carrying anything ungraded, so a broken script
/// still stops the build without discarding every other case in the run.
/// The payload goes over on its own thread so that neither side can wedge the other.
///
/// A grader is free to print more than a pipe holds before it reads its input, and a parent that
/// insists on finishing the write first would then wait on a buffer only the grader can drain
/// while the grader waits on one only the parent can drain. Neither ever gives way.
fn feed_payload(child: &mut std::process::Child, payload: Vec<u8>) -> Option<PayloadHandover> {
    let mut stdin = child.stdin.take()?;
    Some(std::thread::spawn(move || {
        use std::io::Write;
        stdin.write_all(&payload)
    }))
}

type PayloadHandover = std::thread::JoinHandle<std::io::Result<()>>;

/// A grader that stopped reading is answered by its exit status, not by the write that failed.
///
/// A script which exits before draining the payload closes the pipe under us, and reporting that
/// as harness I/O aborts the entire run over one broken grader, discarding every case already
/// paid for. Whatever it exited with is the honest account of what it did.
fn payload_handed_over(handover: Option<PayloadHandover>) -> Result<()> {
    let Some(handover) = handover else {
        return Ok(());
    };
    match handover.join() {
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Ok(other) => other.map_err(EvalError::Io),
        Err(_) => Err(EvalError::Io(std::io::Error::other(
            "the thread handing the payload to the grader script panicked",
        ))),
    }
}

fn script_crashed_result(assertion: &str, command: &str, output: &std::process::Output) -> AssertionGradeResult {
    let evidence = format!(
        "script grader exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.clone(),
        grader: GraderInfo {
            kind: GraderKind::Script,
            model: None,
            command: Some(command.to_string()),
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: Some(evidence),
        votes: None,
    }
}

fn grade_with_script(
    assertion: &str,
    eval_case: &EvalCase,
    ctx: &RunContext,
    options: &GradeOptions,
) -> Result<AssertionGradeResult> {
    let command = options.grader_command.as_deref().ok_or_else(|| {
        EvalError::Validation(ValidationError::for_field("--grader-command", "is required when --grader script").into())
    })?;

    let mut input = serde_json::json!({
        "assertion": assertion,
        "workspace": ctx.workspace_dir,
        "outputs": ctx.outputs_dir,
        "transcript": ctx.transcript_path,
    });
    if let Some(hints) = &eval_case.grader_hints {
        input["grader_hints"] =
            serde_json::Value::Object(hints.iter().map(|(key, value)| (key.clone(), value.clone())).collect());
    }

    let mut child = Command::new(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&ctx.workspace_dir)
        .spawn()
        .map_err(|e| EvalError::Validation(ValidationError::for_field("--grader-command", e.to_string()).into()))?;

    let handover = feed_payload(&mut child, serde_json::to_string(&input)?.into_bytes());
    let output = child.wait_with_output().map_err(EvalError::Io)?;
    payload_handed_over(handover)?;

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let artifact_path = ctx.run_dir.join("grader-script-result.json");
    std::fs::write(
        &artifact_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "assertion": assertion,
            "exit_code": output.status.code(),
            "stdout": raw,
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))?,
    )?;

    if !output.status.success() {
        return Ok(script_crashed_result(assertion, command, &output));
    }

    let parsed: ScriptGraderResponse = serde_json::from_str(&raw).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                "script grader output",
                format!("invalid JSON contract: {e}; expected {{\"passed\": bool, \"evidence\": string, \"rationale\"?: string}}"),
            )
            .into(),
        )
    })?;

    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: parsed.passed,
        evidence: parsed.evidence,
        grader: GraderInfo {
            kind: GraderKind::Script,
            model: None,
            command: Some(command.to_string()),
        },
        rationale: parsed.rationale,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
    })
}

#[derive(Debug, Deserialize)]
struct ScriptGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

fn grade_with_llm(assertion: &str, ctx: &RunContext, session: &GradeSession) -> Result<AssertionGradeResult> {
    if let Some(kind) = parse_mechanical_kind(assertion) {
        let (passed, evidence) = evaluate_mechanical(&kind, ctx)?;
        return Ok(AssertionGradeResult {
            name: None,
            assertion: assertion.to_string(),
            passed,
            evidence,
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
        });
    }

    let endpoint = session.endpoint()?;
    let request = JudgeRequest::new(LLM_GRADER_SYSTEM_PROMPT, llm_grader_payload(assertion, ctx)?);

    let votes = session.options.grader_votes;
    let mut opinions = Vec::with_capacity(votes.count() as usize);
    for _ in votes.ballots() {
        let reply = judge::judge(endpoint, &request, "--grader-model")?;
        let parsed: LlmGraderResponse = judge::parse_json_reply(&reply, "--grader-model")?;
        opinions.push((parsed.passed, parsed));
    }

    let verdict = tally_opinions(opinions).ok_or_else(|| {
        EvalError::Validation(
            ValidationError::for_field("--grader-votes", "no judge opinion was taken for this assertion").into(),
        )
    })?;

    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: verdict.tally.majority_passed(),
        evidence: verdict.opinion.evidence,
        grader: GraderInfo {
            kind: GraderKind::Llm,
            model: Some(endpoint.model.to_string()),
            command: None,
        },
        rationale: verdict.opinion.rationale,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: (!votes.is_single()).then_some(verdict.tally),
    })
}

const LLM_GRADER_SYSTEM_PROMPT: &str = "You are grading one assertion about the output of a single agent run. \
     Decide only whether the assertion holds for the material you are given. \
     Quote the material in `evidence`; do not restate the assertion as its own evidence. \
     Respond with JSON: {\"passed\": true|false, \"evidence\": \"...\", \"rationale\": \"...\"}.";

/// How much of one artifact the judge is shown.
///
/// Bounded so a run that wrote a large file does not turn one assertion into an
/// unbounded request.
const LLM_GRADER_EXCERPT_BYTES: usize = 8_000;

fn llm_grader_payload(assertion: &str, ctx: &RunContext) -> Result<String> {
    let final_text = std::fs::read_to_string(ctx.outputs_dir.join(FINAL_MD)).unwrap_or_default();
    let mut outputs = serde_json::Map::new();
    if let Ok(entries) = std::fs::read_dir(&ctx.outputs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == FINAL_MD {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                outputs.insert(name, serde_json::Value::String(excerpt(&text)));
            }
        }
    }

    Ok(serde_json::to_string(&serde_json::json!({
        "assertion": assertion,
        "final_text": excerpt(&final_text),
        "outputs": outputs,
    }))?)
}

fn excerpt(text: &str) -> String {
    if text.len() <= LLM_GRADER_EXCERPT_BYTES {
        return text.to_string();
    }
    let mut end = LLM_GRADER_EXCERPT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated after {end} bytes]", &text[..end])
}

#[derive(Debug, Deserialize)]
struct LlmGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

/// The assertion as the author wrote it, beside an ASCII-lowercased copy used only to
/// locate keywords.
///
/// Keyword matching has to ignore case; capture must not. Mapping each byte to itself or
/// to exactly one other byte keeps the two copies index-aligned, so an offset found in
/// `lower` names the same position in `original`, and every path, needle and pattern
/// handed back is a slice of what was written. Nothing here can hand out lowercased text,
/// which is the property that was missing when captures came off the lowercase copy.
struct AssertionText {
    original: String,
    lower: String,
}

impl AssertionText {
    fn new(assertion: &str) -> Self {
        let original = assertion.trim().to_string();
        let lower = original.to_ascii_lowercase();
        Self { original, lower }
    }

    fn contains(&self, needle: &str) -> bool {
        self.lower.contains(needle)
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.lower.starts_with(prefix)
    }

    fn ends_with(&self, suffix: &str) -> bool {
        self.lower.ends_with(suffix)
    }

    fn after(&self, prefix: &str) -> Option<&str> {
        let idx = self.lower.find(prefix)? + prefix.len();
        Some(self.original[idx..].trim())
    }

    fn before(&self, suffix: &str) -> Option<&str> {
        let idx = self.lower.find(suffix)?;
        Some(self.original[..idx].trim())
    }

    fn between(&self, prefix: &str, suffix: &str) -> Option<&str> {
        let start = self.lower.find(prefix)? + prefix.len();
        let end = self.lower[start..].find(suffix)? + start;
        Some(self.original[start..end].trim())
    }

    fn unquoted_after(&self, prefix: &str) -> Option<&str> {
        non_empty(unquote(self.after(prefix)?))
    }

    fn unquoted_before(&self, suffix: &str) -> Option<&str> {
        non_empty(unquote(self.before(suffix)?))
    }

    fn unquoted_between(&self, prefix: &str, suffix: &str) -> Option<&str> {
        non_empty(unquote(self.between(prefix, suffix)?))
    }
}

/// One matched pair of surrounding quotes, removed, and only a matched pair.
///
/// Authors quote a path that carries spaces, and each capture site used to decide for
/// itself whether to strip them, so a site that forgot opened a file whose name included
/// the quote characters. Trimming every quote at both ends is what the sites that
/// remembered did, and that also eats an apostrophe the author meant to keep.
fn unquote(value: &str) -> &str {
    let mut chars = value.chars();
    match (chars.next(), chars.next_back()) {
        (Some(open), Some(close)) if open == close && (open == '"' || open == '\'') => chars.as_str(),
        _ => value,
    }
}

fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn parse_mechanical_kind(assertion: &str) -> Option<MechanicalKind> {
    let text = AssertionText::new(assertion);

    if let Some(path) = extract_quoted_or_token_after(&text, "file ", " exists") {
        return Some(MechanicalKind::FileExists { path });
    }
    if text.ends_with(" exists") && !text.contains("file count") && !text.contains("image ") {
        if let Some(path) = extract_path_before(&text, " exists") {
            if !path.contains(' ') {
                return Some(MechanicalKind::FileExists { path });
            }
        }
    }

    if let Some(count) = extract_usize_after(&text, "file count is ") {
        return Some(MechanicalKind::FileCount { count, dir: None });
    }
    if let Some(count) = extract_usize_before(&text, " files") {
        let dir = text.after(" in ").map(str::to_string);
        return Some(MechanicalKind::FileCount { count, dir });
    }
    if let Some(count) = extract_usize_after(&text, "contains ") {
        if text.contains(" files") {
            return Some(MechanicalKind::FileCount { count, dir: None });
        }
    }

    if text.contains("valid json") {
        let path = extract_path_before(&text, " is valid json").or_else(|| extract_path_before(&text, "valid json"));
        return Some(MechanicalKind::ValidJson { path });
    }

    if text.contains("valid csv") {
        let path = extract_path_before(&text, " is valid csv").or_else(|| extract_path_before(&text, "valid csv"));
        return Some(MechanicalKind::ValidCsv { path });
    }

    if text.contains("markdown headings") || text.contains("valid markdown") {
        let path = extract_path_before(&text, " has valid markdown headings");
        return Some(MechanicalKind::ValidMarkdownHeadings { path });
    }

    if let Some(path) = text.after("image exists at ") {
        return Some(MechanicalKind::ImageExists { path: path.to_string() });
    }
    if text.starts_with("image ") && text.ends_with(" exists") {
        if let Some(path) = text.between("image ", " exists") {
            return Some(MechanicalKind::ImageExists { path: path.to_string() });
        }
    }

    if let Some(dims) = extract_dimensions(&text) {
        if let Some(path) = extract_path_before(&text, " is ") {
            return Some(MechanicalKind::ImageDimensions {
                path,
                width: dims.0,
                height: dims.1,
            });
        }
        if text.contains("image dimensions are ") {
            return Some(MechanicalKind::ImageDimensions {
                path: "outputs".to_string(),
                width: dims.0,
                height: dims.1,
            });
        }
    }

    if let Some(needle) = extract_quoted(&text, "contains ") {
        let path = text.after(" in ").map(str::to_string);
        return Some(MechanicalKind::ContainsString { needle, path });
    }
    if let Some(needle) = extract_quoted(&text, "includes ") {
        let path = text.after(" in ").map(str::to_string);
        return Some(MechanicalKind::ContainsString { needle, path });
    }
    if text.starts_with("output includes ") {
        if let Some(rest) = text.unquoted_after("output includes ") {
            return Some(MechanicalKind::ContainsString {
                needle: rest.to_string(),
                path: None,
            });
        }
    }

    if let Some(pattern) = extract_regex_pattern(&text) {
        let path = extract_path_before(&text, " matches");
        return Some(MechanicalKind::MatchesRegex { pattern, path });
    }

    if let Some(count) = extract_usize_after(&text, "row count is ") {
        let path = text.before(" row count").map(str::to_string);
        return Some(MechanicalKind::RowCount { count, path });
    }
    if let Some(count) = extract_usize_before(&text, " rows") {
        let path = text.before(" has ").or_else(|| text.before(" in ")).map(str::to_string);
        return Some(MechanicalKind::RowCount { count, path });
    }

    if text.contains("schema validation") || text.contains("validates against schema ") {
        let schema = text
            .unquoted_between("validates against schema ", " for ")
            .or_else(|| text.unquoted_after("validates against schema "))
            .map(str::to_string);
        let path = text
            .unquoted_after(" for ")
            .map(str::to_string)
            .or_else(|| extract_path_before(&text, " validates against schema"));
        return Some(MechanicalKind::SchemaValidation { schema, path });
    }

    None
}

fn evaluate_mechanical(kind: &MechanicalKind, ctx: &RunContext) -> Result<(bool, String)> {
    match kind {
        MechanicalKind::FileExists { path } => {
            let resolved = resolve_path(ctx, path);
            let exists = resolved.is_file();
            Ok((
                exists,
                if exists {
                    format!("file exists at {}", resolved.display())
                } else {
                    format!("file not found at {}", resolved.display())
                },
            ))
        }
        MechanicalKind::FileCount { count, dir } => {
            let base = dir
                .as_ref()
                .map(|d| resolve_path(ctx, d))
                .unwrap_or_else(|| ctx.outputs_dir.clone());
            let actual = count_files(&base)?;
            Ok((
                actual == *count,
                format!("found {actual} file(s) in {}", base.display()),
            ))
        }
        MechanicalKind::ValidJson { path } => {
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| ctx.outputs_dir.clone());
            Ok(graders::check_json_validity(&graders::TargetContent::from_path(
                &target,
            )))
        }
        MechanicalKind::ValidCsv { path } => {
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| ctx.outputs_dir.clone());
            validate_csv_file(&target)
        }
        MechanicalKind::ValidMarkdownHeadings { path } => {
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| find_first_file_with_extension(&ctx.outputs_dir, "md"));
            validate_markdown_headings(&target)
        }
        MechanicalKind::ImageExists { path } => {
            let resolved = resolve_path(ctx, path);
            let exists = resolved.is_file() && is_image_file(&resolved);
            Ok((
                exists,
                if exists {
                    format!("image exists at {}", resolved.display())
                } else {
                    format!("image not found at {}", resolved.display())
                },
            ))
        }
        MechanicalKind::ImageDimensions { path, width, height } => {
            let resolved = if path == "outputs" {
                find_first_image(&ctx.outputs_dir)
            } else {
                resolve_path(ctx, path)
            };
            match read_image_dimensions(&resolved) {
                Ok((w, h)) => {
                    let passed = w == *width && h == *height;
                    Ok((
                        passed,
                        format!("image at {} is {w}x{h} (expected {width}x{height})", resolved.display()),
                    ))
                }
                Err(msg) => Ok((false, msg)),
            }
        }
        MechanicalKind::ContainsString { needle, path } => {
            let content = if let Some(p) = path {
                std::fs::read_to_string(resolve_path(ctx, p)).unwrap_or_default()
            } else {
                read_search_content(ctx)?
            };
            let found = content.contains(needle);
            Ok((
                found,
                if found {
                    format!("found {:?} in output", needle)
                } else {
                    format!("{:?} not found in output", needle)
                },
            ))
        }
        MechanicalKind::MatchesRegex { pattern, path } => {
            let content = if let Some(p) = path {
                std::fs::read_to_string(resolve_path(ctx, p)).unwrap_or_default()
            } else {
                read_search_content(ctx)?
            };
            let re = Regex::new(pattern).map_err(|e| {
                EvalError::Validation(ValidationError::for_field("assertion regex", e.to_string()).into())
            })?;
            let found = re.is_match(&content);
            Ok((
                found,
                if found {
                    format!("content matches /{pattern}/")
                } else {
                    format!("content does not match /{pattern}/")
                },
            ))
        }
        MechanicalKind::RowCount { count, path } => {
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| find_first_file_with_extension(&ctx.outputs_dir, "csv"));
            let rows = count_csv_rows(&target)?;
            Ok((rows == *count, format!("{} has {rows} data row(s)", target.display())))
        }
        MechanicalKind::SchemaValidation { schema, path } => {
            let schema_name = schema.as_ref().ok_or_else(|| {
                EvalError::Validation(
                    ValidationError::for_field("schema validation", "does not name a schema to validate against")
                        .into(),
                )
            })?;
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| ctx.outputs_dir.join("output.json"));
            let content = graders::TargetContent::from_path(&target);
            graders::check_schema_validation(&ctx.skill_dir.join(schema_name), &content)
        }
    }
}

fn resolve_path(ctx: &RunContext, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if relative.starts_with("outputs/") {
        return ctx.outputs_dir.join(relative.trim_start_matches("outputs/"));
    }
    let in_outputs = ctx.outputs_dir.join(relative);
    if in_outputs.exists() {
        return in_outputs;
    }
    let in_workspace = ctx.workspace_dir.join(relative);
    if in_workspace.exists() {
        return in_workspace;
    }
    ctx.workspace_dir.join(relative)
}

fn count_files(dir: &Path) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

fn validate_csv_file(path: &Path) -> Result<(bool, String)> {
    if !path.is_file() {
        return Ok((false, format!("CSV file not found at {}", path.display())));
    }
    let content = std::fs::read_to_string(path)?;
    let valid = !content.trim().is_empty() && content.lines().all(|line| !line.trim().is_empty() || line.is_empty());
    Ok((
        valid,
        if valid {
            format!("{} is valid CSV", path.display())
        } else {
            format!("{} is empty or invalid CSV", path.display())
        },
    ))
}

fn validate_markdown_headings(path: &Path) -> Result<(bool, String)> {
    if !path.is_file() {
        return Ok((false, format!("Markdown file not found at {}", path.display())));
    }
    let content = std::fs::read_to_string(path)?;
    let has_heading = content.lines().any(|line| line.starts_with('#'));
    Ok((
        has_heading,
        if has_heading {
            format!("{} contains markdown headings", path.display())
        } else {
            format!("{} has no markdown headings", path.display())
        },
    ))
}

fn count_csv_rows(path: &Path) -> Result<usize> {
    if !path.is_file() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<_> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    Ok(lines.len().saturating_sub(1))
}

fn read_search_content(ctx: &RunContext) -> Result<String> {
    let mut parts = Vec::new();
    if ctx.transcript_path.is_file() {
        parts.push(std::fs::read_to_string(&ctx.transcript_path)?);
    }
    if ctx.outputs_dir.is_dir() {
        collect_text_files(&ctx.outputs_dir, &mut parts)?;
    }
    Ok(parts.join("\n"))
}

fn collect_text_files(dir: &Path, parts: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_text_files(&path, parts)?;
        } else if entry.file_type()?.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                parts.push(content);
            }
        }
    }
    Ok(())
}

fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("webp")
    )
}

fn find_first_file_with_extension(dir: &Path, ext: &str) -> PathBuf {
    if !dir.is_dir() {
        return dir.to_path_buf();
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some(ext) {
            return path;
        }
    }
    dir.join(format!("output.{ext}"))
}

fn find_first_image(dir: &Path) -> PathBuf {
    if !dir.is_dir() {
        return dir.to_path_buf();
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_file() && is_image_file(&path) {
            return path;
        }
    }
    dir.join("image.png")
}

fn read_image_dimensions(path: &Path) -> std::result::Result<(u32, u32), String> {
    if !path.is_file() {
        return Err(format!("image not found at {}", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Ok((w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return read_jpeg_dimensions(&bytes);
    }
    Err(format!("unsupported or corrupt image at {}", path.display()))
}

fn read_jpeg_dimensions(bytes: &[u8]) -> std::result::Result<(u32, u32), String> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if matches!(marker, 0xC0..=0xC2) {
            let h = u32::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
            let w = u32::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
            return Ok((w, h));
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        i += 2 + len;
    }
    Err("could not parse JPEG dimensions".to_string())
}

pub fn build_grading_file(assertion_results: Vec<AssertionGradeResult>) -> Result<GradingFile> {
    let summary = GradingCounts::tally(&assertion_results).summary();

    Ok(GradingFile {
        assertion_results,
        summary,
    })
}

pub fn evidence_is_trivial(assertion: &str, evidence: &str) -> bool {
    let a = normalize_for_compare(assertion);
    let e = normalize_for_compare(evidence);
    e.is_empty() || e == a || a.contains(&e) && e.len() > a.len() / 2
}

fn normalize_for_compare(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn validate_grading_document(grading: &GradingFile, strict: bool) -> Result<()> {
    let mut errors = ValidationErrors::new();

    if grading.assertion_results.is_empty() {
        errors.push(ValidationError::for_field(
            "assertion_results",
            "must contain at least one result",
        ));
    }

    let counts = GradingCounts::tally(&grading.assertion_results);
    let passed = counts.passed;
    let failed = counts.failed;

    for (index, result) in grading.assertion_results.iter().enumerate() {
        if result.assertion.trim().is_empty() {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].assertion"),
                "must be a non-empty string",
            ));
        }
        if result.evidence.trim().is_empty() {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].evidence"),
                "must be a non-empty string",
            ));
        }
        if result.passed && result.is_scored() && evidence_is_trivial(&result.assertion, &result.evidence) {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].evidence"),
                "passed assertions must include non-trivial evidence",
            ));
        }
        if strict && result.grader.kind == GraderKind::NeedsLlm {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}]"),
                "requires LLM grading in strict mode",
            ));
        }
    }

    if grading.summary.passed != passed {
        errors.push(ValidationError::for_field(
            "summary.passed",
            format!("{} does not match {passed} passed results", grading.summary.passed),
        ));
    }
    if grading.summary.failed != failed {
        errors.push(ValidationError::for_field(
            "summary.failed",
            format!("{} does not match {failed} failed results", grading.summary.failed),
        ));
    }
    if grading.summary.unsupported != counts.unsupported {
        errors.push(ValidationError::for_field(
            "summary.unsupported",
            format!(
                "{} does not match {} unsupported results",
                grading.summary.unsupported, counts.unsupported
            ),
        ));
    }
    if grading.summary.excluded != counts.excluded {
        errors.push(ValidationError::for_field(
            "summary.excluded",
            format!(
                "{} does not match {} excluded results",
                grading.summary.excluded, counts.excluded
            ),
        ));
    }
    if grading.summary.ungraded != counts.ungraded {
        errors.push(ValidationError::for_field(
            "summary.ungraded",
            format!(
                "{} does not match {} ungraded results",
                grading.summary.ungraded, counts.ungraded
            ),
        ));
    }
    if grading.summary.total != grading.assertion_results.len() {
        errors.push(ValidationError::for_field(
            "summary.total",
            format!(
                "{} does not match {} results",
                grading.summary.total,
                grading.assertion_results.len()
            ),
        ));
    }

    if !pass_rate_matches(grading.summary.pass_rate, counts.pass_rate()) {
        errors.push(ValidationError::for_field(
            "summary.pass_rate",
            format!(
                "{} does not match computed rate {}",
                describe_pass_rate(grading.summary.pass_rate),
                describe_pass_rate(counts.pass_rate())
            ),
        ));
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    Ok(())
}

fn store_grader_artifacts(
    run: &mut RunRecord,
    report_dir: &Path,
    ctx: &RunContext,
    options: &GradeOptions,
    grading: &GradingFile,
) -> Result<()> {
    let grading_relative = ctx
        .run_dir
        .join("grading.json")
        .strip_prefix(report_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "grading.json".to_string());

    run.artifacts.retain(|artifact| {
        artifact
            .get("kind")
            .and_then(|value| value.as_str())
            .is_none_or(|kind| kind != "grading" && kind != "grader_result")
    });

    run.artifacts.push(serde_json::json!({
        "kind": "grading",
        "path": grading_relative,
    }));

    if options.grader == GraderMode::Script {
        let script_result = ctx.run_dir.join("grader-script-result.json");
        if script_result.is_file() {
            let relative = script_result
                .strip_prefix(report_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| script_result.display().to_string());
            run.artifacts.push(serde_json::json!({
                "kind": "grader_result",
                "path": relative,
            }));
        }
    }

    let _ = grading;
    Ok(())
}

/// A graded assertion as it appears in `report.json`: the result exactly as its own run wrote
/// it, plus the two keys that only the run knows. Flattening rather than listing the fields by
/// hand is what keeps a field added to `AssertionGradeResult` later from silently missing the
/// published report the way `name`, `excluded`, `rationale` and `votes` once did.
#[derive(Serialize)]
struct PublishedAssertionResult<'a> {
    run_id: &'a str,
    eval_case_id: &'a str,
    #[serde(flatten)]
    result: &'a AssertionGradeResult,
}

fn update_report_after_grading(
    document: &mut ReportDocument,
    runs: &[RunRecord],
    report_dir: &Path,
    grader_config: &serde_json::Value,
) -> Result<()> {
    document.assertion_results.clear();
    if !document.dimensions.graders.iter().any(|g| g == grader_config) {
        document.dimensions.graders.push(grader_config.clone());
    }

    for run in runs {
        let run_dir = report_dir
            .join(&run.paths.workspace)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| report_dir.to_path_buf());
        let grading_path = run_dir.join("grading.json");
        if !grading_path.is_file() {
            continue;
        }
        let grading: GradingFile = serde_json::from_str(&std::fs::read_to_string(&grading_path)?)?;
        for result in &grading.assertion_results {
            document
                .assertion_results
                .push(serde_json::to_value(PublishedAssertionResult {
                    run_id: &run.id,
                    eval_case_id: &run.eval_case_id,
                    result,
                })?);
        }
    }

    Ok(())
}

fn extract_quoted(text: &AssertionText, prefix: &str) -> Option<String> {
    let rest = text.after(prefix)?;
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some(stripped[..end].to_string());
    }
    if let Some(stripped) = rest.strip_prefix('\'') {
        let end = stripped.find('\'')?;
        return Some(stripped[..end].to_string());
    }
    None
}

fn extract_quoted_or_token_after(text: &AssertionText, prefix: &str, suffix: &str) -> Option<String> {
    text.unquoted_between(prefix, suffix).map(str::to_string)
}

fn extract_path_before(text: &AssertionText, suffix: &str) -> Option<String> {
    text.unquoted_before(suffix).map(str::to_string)
}

fn extract_usize_after(text: &AssertionText, prefix: &str) -> Option<usize> {
    text.after(prefix)?.split_whitespace().next()?.parse().ok()
}

fn extract_usize_before(text: &AssertionText, suffix: &str) -> Option<usize> {
    text.before(suffix)?.split_whitespace().last()?.parse().ok()
}

fn extract_dimensions(text: &AssertionText) -> Option<(u32, u32)> {
    let re = Regex::new(r"(\d+)\s*[x×]\s*(\d+)").ok()?;
    let caps = re.captures(&text.lower)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?))
}

/// The pattern between the `/` delimiters of a `matches` assertion.
///
/// The delimiters are looked for after the keyword rather than from the start of the
/// assertion, because a target path carries slashes of its own. Taking the first pair in
/// the whole string swallowed the path and the keyword into the pattern, and made every
/// later prose form unreachable for any assertion that named a path at all.
fn extract_regex_pattern(text: &AssertionText) -> Option<String> {
    const KEYWORD: &str = "matches";
    let idx = text.lower.find(KEYWORD)? + KEYWORD.len();
    let rest = &text.original[idx..];
    let start = rest.find('/')?;
    let end = rest[start + 1..].find('/')? + start + 1;
    Some(rest[start + 1..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::agentskills::transcript::write_normalized_transcript;
    use crate::fs::testutil::MemFS;
    use std::fs;
    use tempfile::tempdir;

    const UNOBSERVABLE_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used", "arm": "both"},
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

    fn unobservable_report_dir(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let skill_dir = temp.path().join("demo-skill");
        fs::create_dir_all(skill_dir.join("evals")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        )
        .unwrap();
        fs::write(skill_dir.join("evals/evals.json"), UNOBSERVABLE_SUITE).unwrap();

        let mem = MemFS::new();
        let mem_skill = Path::new("demo-skill");
        mem.insert(
            mem_skill.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        mem.insert(mem_skill.join("evals/evals.json"), UNOBSERVABLE_SUITE);

        let bundle = build_report_bundle(
            &mem,
            mem_skill,
            &skill_dir,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("report-unsupported".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                runner: Some("codex".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();

        let workspace = report_dir.join(&bundle.document.runs[0].paths.workspace);
        let run_dir = workspace.parent().unwrap().to_path_buf();
        fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
        let transcript_path = run_dir.join("transcript.jsonl");
        fs::write(&transcript_path, "{\"type\":\"turn.completed\"}\n").unwrap();
        write_normalized_transcript(&transcript_path, &NormalizedTranscript::unavailable("codex")).unwrap();

        (report_dir, run_dir)
    }

    const ENGAGED_STREAM: &[u8] = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/summary.md"}}]}}
{"type":"result","is_error":false,"result":"all done"}
"#;

    const IMPROVISED_STREAM: &[u8] = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/summary.md"}}]}}
{"type":"result","is_error":false,"result":"all done"}
"#;

    /// Both arms of a suite, each run leaving the same final text so the only
    /// thing that differs between them is whether the skill was engaged.
    fn both_arms_report_dir(temp: &tempfile::TempDir, suite_json: &str) -> PathBuf {
        use crate::agentskills::redact::redact_transcript_bytes;
        use crate::agentskills::transcript::{normalize_stream_json, WorkspaceBoundary};

        let skill_dir = temp.path().join("demo-skill");
        fs::create_dir_all(skill_dir.join("evals")).unwrap();
        let skill_md = "---\nname: demo-skill\ndescription: d\n---\n";
        fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
        fs::write(skill_dir.join("evals/evals.json"), suite_json).unwrap();

        let mem = MemFS::new();
        let mem_skill = Path::new("demo-skill");
        mem.insert(mem_skill.join("SKILL.md"), skill_md);
        mem.insert(mem_skill.join("evals/evals.json"), suite_json);

        let bundle = build_report_bundle(
            &mem,
            mem_skill,
            &skill_dir,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions {
                report_id: Some("report-arms".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                runner: Some("claude-code".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();

        for run in &bundle.document.runs {
            let workspace = report_dir.join(&run.paths.workspace);
            let run_dir = workspace.parent().unwrap().to_path_buf();
            fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
            let stream = match run.scenario_id {
                ScenarioKind::WithSkill => ENGAGED_STREAM,
                _ => IMPROVISED_STREAM,
            };
            let transcript_path = run_dir.join("transcript.jsonl");
            fs::write(&transcript_path, stream).unwrap();
            write_normalized_transcript(
                &transcript_path,
                &normalize_stream_json(
                    "claude-code",
                    &redact_transcript_bytes(stream),
                    &WorkspaceBoundary::unknown(),
                ),
            )
            .unwrap();
        }

        report_dir
    }

    fn grading_files_by_scenario(report_dir: &Path) -> Vec<(ScenarioKind, GradingFile)> {
        let document: ReportDocument =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        document
            .runs
            .iter()
            .map(|run| {
                let workspace = report_dir.join(&run.paths.workspace);
                let run_dir = workspace.parent().unwrap();
                let grading: GradingFile =
                    serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
                (run.scenario_id, grading)
            })
            .collect()
    }

    const ARM_SCOPED_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used"},
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

    const TRIGGERING_ONLY_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used"}
                ]
            }
        ]
    }"#;

    #[test]
    fn a_check_that_presupposes_the_skill_scores_in_neither_arm() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.excluded, 2, "one skill_used result per arm");
        assert_eq!(report.assertions_graded, 4);
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 0);

        for (scenario, grading) in grading_files_by_scenario(&report_dir) {
            assert_eq!(grading.summary.excluded, 1, "{scenario:?}");
            assert_eq!(
                grading.summary.pass_rate,
                Some(1.0),
                "{scenario:?} must not be scored on the skill's own premise"
            );
            let skill_result = grading
                .assertion_results
                .iter()
                .find(|result| result.assertion.contains("skill was engaged"))
                .expect("the indicator is still reported");
            assert!(skill_result.is_excluded());
            assert_eq!(skill_result.passed, scenario == ScenarioKind::WithSkill);
        }
    }

    #[test]
    fn a_case_whose_every_check_is_arm_scoped_is_scored_as_declared() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, TRIGGERING_ONLY_SUITE);

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.excluded, 0, "excluding everything would measure nothing");
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);

        for (_, grading) in grading_files_by_scenario(&report_dir) {
            assert_eq!(grading.summary.excluded, 0);
            assert!(grading.summary.pass_rate.is_some());
        }
    }

    const NAMED_GRADER_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done", "name": "wraps-up"}
                ]
            }
        ]
    }"#;

    #[test]
    fn a_named_grader_carries_its_name_into_the_grading_result() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, NAMED_GRADER_SUITE);

        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        for (scenario, grading) in grading_files_by_scenario(&report_dir) {
            let named_result = grading
                .assertion_results
                .iter()
                .find(|result| result.name.as_deref() == Some("wraps-up"))
                .unwrap_or_else(|| panic!("{scenario:?} must report the declared grader name"));
            assert!(named_result.assertion.contains("all done"));
        }
    }

    const NAMED_AND_UNNAMED_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done", "name": "wraps-up"}
                ],
                "assertions": ["the tone stays professional throughout"]
            }
        ]
    }"#;

    /// A grader the author never named must not pick one up along the way,
    /// whether from a sibling grader in the same case or from its own
    /// rendered description: an unnamed grader indistinguishable from a
    /// named one defeats the point of naming one at all.
    #[test]
    fn an_unnamed_prose_assertion_stays_unnamed_next_to_a_named_grader() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, NAMED_AND_UNNAMED_SUITE);

        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        for (scenario, grading) in grading_files_by_scenario(&report_dir) {
            let prose_result = grading
                .assertion_results
                .iter()
                .find(|result| result.assertion.contains("professional"))
                .unwrap_or_else(|| panic!("{scenario:?} must still report the prose assertion"));
            assert_eq!(
                prose_result.name, None,
                "{scenario:?} prose assertion must stay unnamed"
            );
        }

        let published: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        let flattened_prose = published["assertion_results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|result| {
                result["assertion"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("professional")
            })
            .expect("the prose result must survive the flatten");
        assert!(
            flattened_prose.get("name").is_none(),
            "an unnamed grader must not gain a name key in the published report.json"
        );

        let flattened_named = published["assertion_results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|result| result["assertion"].as_str().unwrap_or_default().contains("all done"))
            .expect("the named result must survive the flatten");
        assert_eq!(flattened_named["name"], "wraps-up");
    }

    #[test]
    fn a_named_grader_result_is_absent_from_the_serialized_json_when_unnamed() {
        let result = AssertionGradeResult {
            name: None,
            assertion: "the output includes a summary".to_string(),
            passed: true,
            evidence: "found the summary section".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("\"name\""));
    }

    /// The flatten in `update_report_after_grading` used to rebuild the object
    /// field by field, which is exactly how `name`, `excluded`, `rationale`
    /// and `votes` were previously lost between `grading.json` and the
    /// published `report.json`. This pins every one of those fields against
    /// the artifact a reader actually opens, not just the per-run file.
    #[test]
    fn a_named_excluded_multi_voted_result_survives_the_flatten_into_report_json() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

        let mut document: ReportDocument =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        let runs = document.runs.clone();
        let run = runs.first().expect("both_arms_report_dir produced at least one run");
        let run_dir = report_dir.join(&run.paths.workspace).parent().unwrap().to_path_buf();

        let crafted = GradingFile {
            assertion_results: vec![AssertionGradeResult {
                name: Some("wraps-up".to_string()),
                assertion: "the summary reads well".to_string(),
                passed: true,
                evidence: "the summary names every column".to_string(),
                grader: GraderInfo {
                    kind: GraderKind::Llm,
                    model: Some("judge-model".to_string()),
                    command: None,
                },
                rationale: Some("both judges agreed".to_string()),
                unsupported: None,
                excluded: Some("scoped to the with_skill arm".to_string()),
                ungraded: None,
                votes: Some(JudgeVoteTally { passed: 2, failed: 1 }),
            }],
            summary: GradingSummary {
                passed: 1,
                failed: 0,
                total: 1,
                unsupported: 0,
                excluded: 1,
                ungraded: 0,
                pass_rate: Some(1.0),
            },
        };
        fs::write(
            run_dir.join("grading.json"),
            serde_json::to_string_pretty(&crafted).unwrap(),
        )
        .unwrap();

        let grader_config = build_grader_config(&GradeOptions::default());
        update_report_after_grading(&mut document, &runs, &report_dir, &grader_config).unwrap();
        std::fs::write(
            report_dir.join("report.json"),
            serde_json::to_string_pretty(&document).unwrap(),
        )
        .unwrap();

        let published: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        let flattened = published["assertion_results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|result| result["assertion"] == "the summary reads well")
            .expect("the crafted result must survive the flatten into the published report.json");

        assert_eq!(flattened["name"], "wraps-up");
        assert_eq!(flattened["excluded"], "scoped to the with_skill arm");
        assert_eq!(flattened["rationale"], "both judges agreed");
        assert_eq!(flattened["votes"]["passed"], 2);
        assert_eq!(flattened["votes"]["failed"], 1);
    }

    fn suite_from(json: &str) -> EvalSuite {
        crate::agentskills::evals::parse_eval_suite(json).unwrap()
    }

    #[test]
    fn the_recorded_grader_config_tells_a_panel_from_a_single_opinion() {
        let single = build_grader_config(&GradeOptions::default());
        let panel = build_grader_config(&GradeOptions {
            grader_votes: JudgeVotes::parse(3).unwrap(),
            ..GradeOptions::default()
        });

        assert_eq!(single["votes"], serde_json::json!(1));
        assert_eq!(panel["votes"], serde_json::json!(3));
    }

    #[test]
    fn a_suite_of_typed_graders_needs_no_judge_under_auto() {
        let suite = suite_from(UNOBSERVABLE_SUITE);
        let options = GradeOptions {
            grader: GraderMode::Auto,
            ..GradeOptions::default()
        };

        assert!(!suite_needs_a_judge(&options, &suite));
        assert!(GradeSession::open(&options, &suite).unwrap().judge.is_none());
    }

    #[test]
    fn a_declared_llm_grader_needs_a_judge_under_auto() {
        let suite = suite_from(
            r#"{
                "schema_version": 3,
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "llm", "criterion": "the summary reads well"}]
                    }
                ]
            }"#,
        );
        let options = GradeOptions {
            grader: GraderMode::Auto,
            ..GradeOptions::default()
        };

        assert!(suite_needs_a_judge(&options, &suite));
        let err = GradeSession::open(&options, &suite).unwrap_err();
        assert!(err.to_string().contains("--grader-model"), "{err}");
    }

    #[test]
    fn a_free_text_assertion_no_mechanical_pattern_recognizes_needs_a_judge_under_auto() {
        let suite = suite_from(
            r#"{
                "schema_version": 3,
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "assertions": ["the tone is appropriate for an executive"]
                    }
                ]
            }"#,
        );
        let options = GradeOptions {
            grader: GraderMode::Auto,
            ..GradeOptions::default()
        };

        assert!(suite_needs_a_judge(&options, &suite));
    }

    #[test]
    fn script_mode_never_resolves_a_judge() {
        let suite = suite_from(
            r#"{
                "schema_version": 3,
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "llm", "criterion": "the summary reads well"}]
                    }
                ]
            }"#,
        );
        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_command: Some("./grade.sh".to_string()),
            ..GradeOptions::default()
        };

        assert!(!suite_needs_a_judge(&options, &suite));
    }

    #[test]
    fn pass_rate_ignores_unsupported_results_so_an_unobservable_runner_cannot_lower_it() {
        let counts = GradingCounts::tally(&[
            AssertionGradeResult {
                name: None,
                assertion: "contains 'all done'".to_string(),
                passed: true,
                evidence: "matched".to_string(),
                grader: GraderInfo {
                    kind: GraderKind::Declarative,
                    model: None,
                    command: None,
                },
                rationale: None,
                unsupported: None,
                excluded: None,
                ungraded: None,
                votes: None,
            },
            AssertionGradeResult {
                name: None,
                assertion: "the skill was engaged".to_string(),
                passed: false,
                evidence: String::new(),
                grader: GraderInfo {
                    kind: GraderKind::Declarative,
                    model: None,
                    command: None,
                },
                rationale: None,
                unsupported: Some("runner 'codex' does not expose tool calls".to_string()),
                excluded: None,
                ungraded: None,
                votes: None,
            },
        ]);

        assert_eq!(counts.total, 2);
        assert_eq!(counts.unsupported, 1);
        assert_eq!(counts.passed, 1);
        assert_eq!(counts.failed, 0);
        assert_eq!(counts.scored(), 1);
        assert_eq!(counts.pass_rate(), Some(1.0));
    }

    #[test]
    fn a_summary_with_nothing_scored_reports_no_pass_rate_rather_than_zero() {
        let counts = GradingCounts::tally(&[AssertionGradeResult {
            name: None,
            assertion: "the skill was engaged".to_string(),
            passed: false,
            evidence: "runner 'codex' does not expose tool calls".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Declarative,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: Some("runner 'codex' does not expose tool calls".to_string()),
            excluded: None,
            ungraded: None,
            votes: None,
        }]);

        assert_eq!(counts.scored(), 0);
        assert_eq!(counts.pass_rate(), None);

        let summary = counts.summary();
        assert_eq!(summary.pass_rate, None);
        assert_eq!(
            serde_json::to_value(&summary).unwrap()["pass_rate"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_grade_tally_names_the_runs_that_never_completed() {
        let mut statuses = GradedRunStatuses::default();
        for status in ["completed", "failed", "timeout", "skipped", "exploded"] {
            statuses.record(status);
        }

        assert_eq!(statuses.completed, 1);
        assert_eq!(statuses.not_completed(), 4);
        assert_eq!(
            statuses.describe_not_completed().as_deref(),
            Some("1 failed, 1 timed out, 1 skipped, 1 of an unrecognized status")
        );

        let clean = GradedRunStatuses {
            completed: 3,
            ..GradedRunStatuses::default()
        };
        assert_eq!(clean.not_completed(), 0);
        assert_eq!(clean.describe_not_completed(), None);
    }

    fn result_for_test(assertion: &str, passed: bool) -> AssertionGradeResult {
        AssertionGradeResult {
            name: None,
            assertion: assertion.to_string(),
            passed,
            evidence: "e".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
        }
    }

    #[test]
    fn an_assertion_nothing_could_grade_is_not_counted_as_a_failure() {
        let results = vec![
            result_for_test("a", true),
            needs_llm_result("b", "grader mode is none, so no judge was consulted"),
        ];

        let counts = GradingCounts::tally(&results);

        assert_eq!(counts.ungraded, 1);
        assert_eq!(
            counts.failed, 0,
            "an assertion no grader could attempt is not evidence the skill failed it"
        );
        assert_eq!(counts.passed, 1);
        assert_eq!(counts.scored(), 1);

        let summary = counts.summary();
        assert_eq!(summary.ungraded, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(
            summary.pass_rate,
            Some(1.0),
            "pass_rate is over the scored assertions, so an ungraded one cannot dilute it"
        );
    }

    /// An arm-scoped check that nothing could attempt carries the excluded and the
    /// ungraded marker at once, so every count taken over those markers has to agree
    /// on which one wins. Excluded wins, because a check nobody was going to score
    /// loses nothing by also having been unanswerable. The list and the number are
    /// asserted together: letting them drift is how a report names assertions that
    /// the figure printed beside them does not count.
    #[test]
    fn an_excluded_assertion_that_nothing_attempted_is_counted_and_listed_the_same_way() {
        let mut unattempted_and_out_of_scope = needs_llm_result("b", "no judge was consulted");
        unattempted_and_out_of_scope.excluded = Some("scored in the with-skill arm only".to_string());
        let results = vec![
            result_for_test("a", true),
            unattempted_and_out_of_scope,
            needs_llm_result("c", "no judge was consulted"),
        ];

        assert!(
            results[1].is_ungraded(),
            "the ungraded marker is still there for a second predicate to misread"
        );
        assert_eq!(results[1].outcome(), AssertionOutcome::Excluded);

        let counts = GradingCounts::tally(&results);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.ungraded, 1, "only the check that was in scope went unmeasured");
        assert_eq!(counts.failed, 0);

        let listed = ungraded_assertion_texts(&results);
        assert_eq!(
            listed.len(),
            counts.ungraded,
            "the assertions listed as ungraded must be the ones the count counted"
        );
        assert_eq!(listed, vec!["c".to_string()]);
    }

    #[test]
    fn every_assertion_going_ungraded_leaves_no_pass_rate_at_all() {
        let results = vec![needs_llm_result("a", "no judge was consulted")];

        let counts = GradingCounts::tally(&results);

        assert_eq!(counts.ungraded, 1);
        assert_eq!(counts.failed, 0);
        assert_eq!(counts.scored(), 0);
        assert_eq!(
            counts.pass_rate(),
            None,
            "nothing answered is not the same result as everything failed"
        );
    }

    #[test]
    fn grading_a_run_from_an_unobservable_runner_reports_unsupported_instead_of_failed() {
        let temp = tempdir().unwrap();
        let (report_dir, run_dir) = unobservable_report_dir(&temp);

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                grader_provider: JudgeProvider::default(),
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.assertions_graded, 2);
        assert_eq!(report.unsupported, 1);
        assert_eq!(report.excluded, 0);
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.ungraded, 0);
        assert_eq!(report.run_statuses.skipped, 1);
        assert_eq!(report.run_statuses.completed, 0);
        assert_eq!(
            report.run_statuses.describe_not_completed().as_deref(),
            Some("1 skipped")
        );

        let grading: GradingFile =
            serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
        assert_eq!(grading.summary.total, 2);
        assert_eq!(grading.summary.unsupported, 1);
        assert_eq!(grading.summary.passed, 1);
        assert_eq!(grading.summary.failed, 0);
        assert_eq!(grading.summary.pass_rate, Some(1.0));

        let unsupported = grading
            .assertion_results
            .iter()
            .find(|result| result.is_unsupported())
            .unwrap();
        assert!(unsupported.unsupported.as_deref().unwrap().contains("codex"));

        let document: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        let flattened = document["assertion_results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|result| result.get("unsupported").is_some())
            .expect("report.json carries the unsupported reason forward");
        assert_eq!(flattened["passed"], false);
        assert!(flattened["unsupported"].as_str().unwrap().contains("codex"));
    }

    fn ctx_with_outputs(dir: &Path) -> RunContext {
        let run_dir = dir.join("runs/run-001");
        let workspace = run_dir.join("workspace");
        let outputs = run_dir.join("outputs");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outputs).unwrap();
        RunContext {
            run_dir: run_dir.clone(),
            workspace_dir: workspace,
            outputs_dir: outputs,
            transcript_path: run_dir.join("transcript.jsonl"),
            skill_dir: dir.join("skill"),
        }
    }

    #[test]
    fn parse_file_exists_patterns() {
        assert!(matches!(
            parse_mechanical_kind(r#"file "out.json" exists"#),
            Some(MechanicalKind::FileExists { .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("outputs/report.md exists"),
            Some(MechanicalKind::FileExists { .. })
        ));
    }

    #[test]
    fn parse_file_count_patterns() {
        assert!(matches!(
            parse_mechanical_kind("file count is 3"),
            Some(MechanicalKind::FileCount { count: 3, .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("contains 2 files"),
            Some(MechanicalKind::FileCount { count: 2, .. })
        ));
    }

    #[test]
    fn parse_valid_json_and_csv() {
        assert!(matches!(
            parse_mechanical_kind("out.json is valid json"),
            Some(MechanicalKind::ValidJson { .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("data.csv is valid csv"),
            Some(MechanicalKind::ValidCsv { .. })
        ));
    }

    #[test]
    fn parse_markdown_headings() {
        assert!(matches!(
            parse_mechanical_kind("report.md has valid markdown headings"),
            Some(MechanicalKind::ValidMarkdownHeadings { .. })
        ));
    }

    #[test]
    fn parse_image_patterns() {
        assert!(matches!(
            parse_mechanical_kind("image chart.png exists"),
            Some(MechanicalKind::ImageExists { .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("chart.png is 800x600"),
            Some(MechanicalKind::ImageDimensions {
                width: 800,
                height: 600,
                ..
            })
        ));
    }

    #[test]
    fn parse_contains_and_regex() {
        assert!(matches!(
            parse_mechanical_kind(r#"contains "hello""#),
            Some(MechanicalKind::ContainsString { .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("matches regex /foo.*/"),
            Some(MechanicalKind::MatchesRegex { .. })
        ));
    }

    #[test]
    fn parse_row_count_and_schema() {
        assert!(matches!(
            parse_mechanical_kind("row count is 10"),
            Some(MechanicalKind::RowCount { count: 10, .. })
        ));
        assert!(matches!(
            parse_mechanical_kind("validates against schema output"),
            Some(MechanicalKind::SchemaValidation { .. })
        ));
    }

    #[test]
    fn mechanical_file_exists_passes_and_fails() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.json"), "{}").unwrap();

        let kind = MechanicalKind::FileExists {
            path: "out.json".to_string(),
        };
        let (passed, evidence) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);
        assert!(evidence.contains("exists"));

        let kind = MechanicalKind::FileExists {
            path: "missing.json".to_string(),
        };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(!passed);
    }

    #[test]
    fn mechanical_file_count() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("a.txt"), "a").unwrap();
        fs::write(ctx.outputs_dir.join("b.txt"), "b").unwrap();

        let kind = MechanicalKind::FileCount { count: 2, dir: None };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);
    }

    #[test]
    fn mechanical_valid_json() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.json"), r#"{"ok": true}"#).unwrap();

        let kind = MechanicalKind::ValidJson {
            path: Some("out.json".to_string()),
        };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);
    }

    #[test]
    fn parse_schema_validation_separates_the_schema_name_from_the_target_clause() {
        let kind = parse_mechanical_kind("output.json validates against schema report for outputs/output.json");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation {
                schema: Some(ref schema),
                path: Some(ref path),
            }) if schema == "report" && path == "outputs/output.json"
        ));
    }

    #[test]
    fn parse_schema_validation_for_a_path_alone_names_no_schema() {
        let kind = parse_mechanical_kind("schema validation for outputs/output.json");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation { schema: None, .. })
        ));
    }

    #[test]
    fn a_prose_schema_form_grades_the_target_it_names() {
        let kind = parse_mechanical_kind("outputs/report.json validates against schema schemas/report.schema.json");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation {
                schema: Some(ref schema),
                path: Some(ref path),
            }) if schema == "schemas/report.schema.json" && path == "outputs/report.json"
        ));
    }

    #[test]
    fn a_quoted_prose_schema_name_is_read_without_its_quotes() {
        let kind = parse_mechanical_kind("outputs/report.json validates against schema \"schemas/Report.schema.json\"");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation {
                schema: Some(ref schema),
                path: Some(ref path),
            }) if schema == "schemas/Report.schema.json" && path == "outputs/report.json"
        ));
    }

    #[test]
    fn a_quoted_prose_schema_target_is_read_without_its_quotes() {
        let kind = parse_mechanical_kind("validates against schema report.schema.json for \"outputs/a report.json\"");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation {
                schema: Some(ref schema),
                path: Some(ref path),
            }) if schema == "report.schema.json" && path == "outputs/a report.json"
        ));
    }

    /// An apostrophe inside a name is not a quote to strip, and a lone leading quote is not
    /// a pair. Trimming every quote character at both ends got both of these wrong.
    #[test]
    fn unquoting_a_captured_name_leaves_an_unpaired_quote_alone() {
        assert_eq!(unquote("\"schema.json\""), "schema.json");
        assert_eq!(unquote("'schema.json'"), "schema.json");
        assert_eq!(unquote("\"schema.json"), "\"schema.json");
        assert_eq!(unquote("yordis's report.json"), "yordis's report.json");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn a_prose_schema_name_and_target_keep_the_case_the_author_wrote() {
        let kind = parse_mechanical_kind("outputs/Report.json validates against schema schemas/Report.schema.json");
        assert!(matches!(
            kind,
            Some(MechanicalKind::SchemaValidation {
                schema: Some(ref schema),
                path: Some(ref path),
            }) if schema == "schemas/Report.schema.json" && path == "outputs/Report.json"
        ));
    }

    #[test]
    fn a_prose_target_keeps_the_case_the_author_wrote() {
        let kind = parse_mechanical_kind(r#"contains "Hello World" in outputs/Report.md"#);
        assert!(matches!(
            kind,
            Some(MechanicalKind::ContainsString {
                ref needle,
                path: Some(ref path),
            }) if needle == "Hello World" && path == "outputs/Report.md"
        ));
    }

    #[test]
    fn a_prose_regex_keeps_the_case_the_author_wrote() {
        let kind = parse_mechanical_kind("outputs/Report.md matches /Error [0-9]+/");
        assert!(matches!(
            kind,
            Some(MechanicalKind::MatchesRegex {
                ref pattern,
                path: Some(ref path),
            }) if pattern == "Error [0-9]+" && path == "outputs/Report.md"
        ));
    }

    #[test]
    fn mechanical_schema_validation_is_an_authoring_error_when_no_schema_is_named() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("output.json"), r#"{"ok": true}"#).unwrap();

        let kind = MechanicalKind::SchemaValidation {
            schema: None,
            path: Some("output.json".to_string()),
        };
        let err = evaluate_mechanical(&kind, &ctx).unwrap_err();
        assert!(err.to_string().contains("does not name a schema"), "{err}");
    }

    #[test]
    fn mechanical_schema_validation_resolves_the_schema_against_the_skill_directory() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::create_dir_all(&ctx.skill_dir).unwrap();
        fs::write(
            ctx.skill_dir.join("report.schema.json"),
            r#"{"type": "object", "required": ["ok"]}"#,
        )
        .unwrap();
        fs::write(ctx.outputs_dir.join("output.json"), r#"{"ok": true}"#).unwrap();

        let kind = MechanicalKind::SchemaValidation {
            schema: Some("report.schema.json".to_string()),
            path: Some("output.json".to_string()),
        };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);

        fs::write(ctx.outputs_dir.join("output.json"), r#"{"nope": true}"#).unwrap();
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(!passed);
    }

    #[test]
    fn mechanical_valid_csv() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("data.csv"), "a,b\n1,2").unwrap();

        let kind = MechanicalKind::ValidCsv {
            path: Some("data.csv".to_string()),
        };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);
    }

    #[test]
    fn mechanical_markdown_headings() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("report.md"), "# Title\n\nBody").unwrap();

        let kind = MechanicalKind::ValidMarkdownHeadings {
            path: Some("report.md".to_string()),
        };
        let (passed, _) = evaluate_mechanical(&kind, &ctx).unwrap();
        assert!(passed);
    }

    #[test]
    fn mechanical_image_exists_and_dimensions() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        // Minimal 1x1 PNG
        let png: [u8; 33] = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
        ];
        fs::write(ctx.outputs_dir.join("chart.png"), png).unwrap();

        let exists = MechanicalKind::ImageExists {
            path: "chart.png".to_string(),
        };
        assert!(evaluate_mechanical(&exists, &ctx).unwrap().0);

        let dims = MechanicalKind::ImageDimensions {
            path: "chart.png".to_string(),
            width: 1,
            height: 1,
        };
        assert!(evaluate_mechanical(&dims, &ctx).unwrap().0);
    }

    #[test]
    fn mechanical_contains_string() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.txt"), "hello world").unwrap();

        let kind = MechanicalKind::ContainsString {
            needle: "hello".to_string(),
            path: Some("out.txt".to_string()),
        };
        assert!(evaluate_mechanical(&kind, &ctx).unwrap().0);
    }

    #[test]
    fn mechanical_regex_match() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.txt"), "foo123").unwrap();

        let kind = MechanicalKind::MatchesRegex {
            pattern: "foo\\d+".to_string(),
            path: Some("out.txt".to_string()),
        };
        assert!(evaluate_mechanical(&kind, &ctx).unwrap().0);
    }

    #[test]
    fn mechanical_row_count() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("data.csv"), "h1,h2\n1,2\n3,4").unwrap();

        let kind = MechanicalKind::RowCount {
            count: 2,
            path: Some("data.csv".to_string()),
        };
        assert!(evaluate_mechanical(&kind, &ctx).unwrap().0);
    }

    #[test]
    fn evidence_is_trivial_rejects_restatement() {
        assert!(evidence_is_trivial(
            "The output includes a summary",
            "The output includes a summary"
        ));
        assert!(evidence_is_trivial("includes summary", ""));
        assert!(!evidence_is_trivial(
            "includes summary",
            "found summary section in outputs/report.md"
        ));
    }

    #[test]
    fn validate_grading_rejects_trivial_pass_evidence() {
        let grading = GradingFile {
            assertion_results: vec![AssertionGradeResult {
                name: None,
                assertion: "file out.json exists".to_string(),
                passed: true,
                evidence: "file out.json exists".to_string(),
                grader: GraderInfo {
                    kind: GraderKind::Mechanical,
                    model: None,
                    command: None,
                },
                rationale: None,
                unsupported: None,
                excluded: None,
                ungraded: None,
                votes: None,
            }],
            summary: GradingSummary {
                passed: 1,
                failed: 0,
                total: 1,
                unsupported: 0,
                excluded: 0,
                ungraded: 0,
                pass_rate: Some(1.0),
            },
        };

        assert!(validate_grading_document(&grading, false).is_err());
    }

    #[test]
    fn script_grader_integration() {
        let tmp = tempdir().unwrap();
        let script = tmp.path().join("fake-grader.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
read input
echo '{"passed": true, "evidence": "script verified workspace contents", "rationale": "ok"}'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.workspace_dir.join("done.txt"), "ok").unwrap();

        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_provider: JudgeProvider::default(),
            grader_model: None,
            grader_command: Some(script.to_string_lossy().into_owned()),
            grader_votes: JudgeVotes::single(),
            strict: false,
        };

        let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case",
            "prompt": "prompt long enough",
            "expected_output": "expected output",
            "assertions": ["custom assertion"],
        }))
        .unwrap();

        let result = grade_with_script("custom assertion", &eval_case, &ctx, &options).unwrap();
        assert!(result.passed);
        assert_eq!(result.grader.kind, GraderKind::Script);
        assert!(result.evidence.contains("script verified"));
    }

    /// A crash is not a verdict. Counting a broken grader as a failed assertion reports that
    /// the skill did the wrong thing on the strength of evidence that says nothing about the
    /// skill at all.
    #[test]
    fn a_crashed_grader_script_grades_nothing_rather_than_failing_the_assertion() {
        let tmp = tempdir().unwrap();
        let script = tmp.path().join("broken-grader.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
echo 'grader could not reach the model' >&2
exit 3
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let ctx = ctx_with_outputs(tmp.path());
        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_provider: JudgeProvider::default(),
            grader_model: None,
            grader_command: Some(script.to_string_lossy().into_owned()),
            grader_votes: JudgeVotes::single(),
            strict: false,
        };
        let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case",
            "prompt": "prompt long enough",
            "expected_output": "expected output",
            "assertions": ["custom assertion"],
        }))
        .unwrap();

        let result = grade_with_script("custom assertion", &eval_case, &ctx, &options).unwrap();

        assert!(
            result.is_ungraded(),
            "a grader that never exited cleanly attempted nothing"
        );
        assert!(!result.is_scored());
        assert!(result.evidence.contains("grader could not reach the model"));

        let counts = GradingCounts::tally(std::slice::from_ref(&result));
        assert_eq!(counts.ungraded, 1);
        assert_eq!(counts.failed, 0, "a broken grader must not read as the skill failing");
    }

    /// A payload too large for the pipe buffer cannot be handed over without the script reading
    /// it, so the script exiting first closes the pipe every time rather than only when it wins
    /// a race. That is the shape a real grader crash takes, and it must still reach a verdict.
    #[test]
    fn a_grader_that_exits_without_reading_its_payload_is_answered_by_its_exit_status() {
        let tmp = tempdir().unwrap();
        let script = tmp.path().join("deaf-grader.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
echo 'grader refused the payload' >&2
exit 4
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let ctx = ctx_with_outputs(tmp.path());
        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_provider: JudgeProvider::default(),
            grader_model: None,
            grader_command: Some(script.to_string_lossy().into_owned()),
            grader_votes: JudgeVotes::single(),
            strict: false,
        };
        let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case",
            "prompt": "prompt long enough",
            "expected_output": "expected output",
            "assertions": ["custom assertion"],
            "grader_hints": { "blob": "x".repeat(512 * 1024) },
        }))
        .unwrap();

        let result = grade_with_script("custom assertion", &eval_case, &ctx, &options).unwrap();

        assert!(
            result.is_ungraded(),
            "a grader that never exited cleanly attempted nothing"
        );
        assert!(result.evidence.contains("grader refused the payload"));
    }

    /// Both pipes are filled past what they hold, in the one order that wedges a parent which
    /// insists on finishing the write before it starts reading. The deadline is the assertion:
    /// a regression here hangs rather than fails, and a hung test reports nothing at all.
    #[test]
    fn a_grader_printing_more_than_a_pipe_holds_does_not_wedge_the_harness() {
        let tmp = tempdir().unwrap();
        let script = tmp.path().join("loud-grader.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
awk 'BEGIN { while (i++ < 40000) print "padding for the stderr pipe" }' >&2
echo '{"passed": true, "evidence": "script verified"}'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let outputs = tmp.path().to_path_buf();
        let command = script.to_string_lossy().into_owned();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ctx = ctx_with_outputs(&outputs);
            let options = GradeOptions {
                grader: GraderMode::Script,
                grader_provider: JudgeProvider::default(),
                grader_model: None,
                grader_command: Some(command),
                grader_votes: JudgeVotes::single(),
                strict: false,
            };
            let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
                "id": "case",
                "prompt": "prompt long enough",
                "expected_output": "expected output",
                "assertions": ["custom assertion"],
                "grader_hints": { "blob": "x".repeat(512 * 1024) },
            }))
            .unwrap();
            let _ = done.send(grade_with_script("custom assertion", &eval_case, &ctx, &options).map(|r| r.passed));
        });

        match finished.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(Ok(passed)) => assert!(passed, "the grader reported a pass once both pipes drained"),
            Ok(Err(e)) => panic!("grading the loud script failed: {e}"),
            Err(_) => panic!("the harness and the grader each waited on a pipe only the other could drain"),
        }
    }

    fn result_with_votes(votes: Option<JudgeVoteTally>) -> AssertionGradeResult {
        AssertionGradeResult {
            name: None,
            assertion: "the summary reads well".to_string(),
            passed: true,
            evidence: "the summary names every column".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Llm,
                model: Some("judge-model".to_string()),
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes,
        }
    }

    /// A reader who never asked for a panel should see the same grading.json they saw
    /// before panels existed, so the field has to stay away rather than report a
    /// one-vote panel that says nothing.
    #[test]
    fn a_result_no_panel_decided_reports_no_split() {
        let json = serde_json::to_value(result_with_votes(None)).unwrap();
        assert!(json.get("votes").is_none());
    }

    #[test]
    fn a_result_a_divided_panel_decided_carries_the_split() {
        let json = serde_json::to_value(result_with_votes(Some(JudgeVoteTally { passed: 2, failed: 1 }))).unwrap();
        assert_eq!(json["votes"]["passed"], 2);
        assert_eq!(json["votes"]["failed"], 1);

        let parsed: AssertionGradeResult = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.votes, Some(JudgeVoteTally { passed: 2, failed: 1 }));
        assert!(!parsed.votes.unwrap().is_unanimous());
    }

    /// A grading.json written before panels existed still has to parse, since a report
    /// bundle outlives the pass that produced it.
    #[test]
    fn a_result_written_before_panels_existed_still_parses() {
        let parsed: AssertionGradeResult = serde_json::from_value(serde_json::json!({
            "assertion": "the summary reads well",
            "passed": true,
            "evidence": "the summary names every column",
            "grader": {"kind": "llm", "model": "judge-model"}
        }))
        .unwrap();
        assert!(parsed.votes.is_none());
    }

    const READ_ONLY_FIXTURE_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

    fn single_run_report_dir(temp: &tempfile::TempDir) -> PathBuf {
        let skill_dir = temp.path().join("demo-skill");
        fs::create_dir_all(skill_dir.join("evals")).unwrap();
        let skill_md = "---\nname: demo-skill\ndescription: d\n---\n";
        fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
        fs::write(skill_dir.join("evals/evals.json"), READ_ONLY_FIXTURE_SUITE).unwrap();

        let mem = MemFS::new();
        let mem_skill = Path::new("demo-skill");
        mem.insert(mem_skill.join("SKILL.md"), skill_md);
        mem.insert(mem_skill.join("evals/evals.json"), READ_ONLY_FIXTURE_SUITE);

        let bundle = build_report_bundle(
            &mem,
            mem_skill,
            &skill_dir,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("report-read-only".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                runner: Some("codex".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();
        let workspace = report_dir.join(&bundle.document.runs[0].paths.workspace);
        let run_dir = workspace.parent().unwrap().to_path_buf();
        fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
        let transcript_path = run_dir.join("transcript.jsonl");
        fs::write(&transcript_path, "{\"type\":\"turn.completed\"}\n").unwrap();
        write_normalized_transcript(&transcript_path, &NormalizedTranscript::unavailable("codex")).unwrap();

        report_dir
    }

    /// The run-level outcome field is what the grading layer turns into a failed case: this
    /// proves that path end to end, including that the case cannot pass on the strength of
    /// its own grader once a read-only fixture it was handed came back changed.
    #[test]
    fn a_read_only_fixture_violation_fails_the_case_even_when_its_own_grader_passes() {
        let temp = tempdir().unwrap();
        let report_dir = single_run_report_dir(&temp);

        let report_path = report_dir.join("report.json");
        let mut document: ReportDocument = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
        document.runs[0].read_only_fixture_violations = vec!["evals/files/input.csv".to_string()];
        fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.passed, 1, "the case's own grader still passes");
        assert_eq!(
            report.failed, 1,
            "a mutated read-only fixture fails the case regardless of what its own checks found"
        );

        let (_, grading) = &grading_files_by_scenario(&report_dir)[0];
        let violation = grading
            .assertion_results
            .iter()
            .find(|result| result.evidence.contains("evals/files/input.csv"))
            .expect("the operator sees the fixture path, not just a count");
        assert!(!violation.passed);
    }
}
