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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::compare::{parse_winner, shuffle_swap, BlindLabel, ComparisonWinner};
use super::evals::{EvalCase, EvalError, EvalSuite, Result};
use super::graders::{
    self, BaselineReference, CaseGrader, GradeInput, GradeTarget, Grader, GraderOutcome, GraderWeight, TargetContent,
    TargetDeclaration,
};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
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
    /// How much this result counts toward its case's score, relative to the
    /// rest of the case. Absent whenever the declaring grader left it
    /// unweighted, so an undeclared weight cannot be told apart in the
    /// serialized form from a build that predates weighting at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<GraderWeight>,
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

    pub fn effective_weight(&self) -> GraderWeight {
        self.weight.unwrap_or_default()
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

/// A case's own score: the fraction of scored weight it passed, `None` under
/// the same condition `GradingCounts::pass_rate` reports `None` under, since
/// weighting which scored assertions count for more cannot manufacture a
/// score out of a case that scored nothing.
///
/// A case where every grader left its weight undeclared takes the same code
/// path `GradingCounts::pass_rate` always has, so a suite that never opts
/// into weighting reports byte-identical scores to before weighting existed.
pub fn case_score(results: &[AssertionGradeResult]) -> Option<f64> {
    if results.iter().all(|r| r.weight.is_none()) {
        return GradingCounts::tally(results).pass_rate();
    }

    let total_weight: f64 = results
        .iter()
        .filter(|r| r.is_scored())
        .map(|r| r.effective_weight().value())
        .sum();
    if total_weight == 0.0 {
        return None;
    }
    let passed_weight: f64 = results
        .iter()
        .filter(|r| r.is_scored() && r.passed)
        .map(|r| r.effective_weight().value())
        .sum();
    Some(passed_weight / total_weight)
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
    /// Outputs-relative paths the run created, read from the artifact index
    /// already persisted onto the run record rather than walked again here.
    created_files: Vec<String>,
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
                .any(|declared| matches!(declared.grader, Grader::Llm { .. } | Grader::Baseline { .. }))
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

        if !run.started() {
            report.run_statuses.record(&run.status);
            continue;
        }

        let ctx = run_context(report_dir, run, &skill_path);
        let mut assertion_results =
            Vec::with_capacity(case.assertions.len() + case.graders.len() + run.mock_violations.len());

        let declarative = DeclarativeContext::load(&ctx);
        for grader in &case.graders {
            assertion_results.push(grade_declaratively(grader, case, &declarative, &ctx, &session)?);
        }

        for assertion in &case.assertions {
            assertion_results.push(grade_assertion(assertion.as_str(), case, &declarative, &ctx, &session)?);
        }

        // A read-only fixture that was modified during the run is a failure of the run
        // itself, not a property left for an assertion or grader to notice, so it is added
        // here rather than left to whichever declared checks the case happens to have.
        for path in &run.read_only_fixture_violations {
            assertion_results.push(read_only_fixture_violation_result(path));
        }

        for violation in &run.mock_violations {
            assertion_results.push(mock_violation_result(violation));
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
        run_mut.case_score = case_score(&assertion_results);

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
    let created_files = created_files_from_artifacts(run);

    RunContext {
        run_dir,
        workspace_dir,
        outputs_dir,
        transcript_path,
        skill_dir: skill_path.to_path_buf(),
        created_files,
    }
}

/// Recovers outputs-relative filenames from the artifact index the run already
/// recorded, so `GradeTarget::CreatedFiles` does not re-walk the output directory.
fn created_files_from_artifacts(run: &RunRecord) -> Vec<String> {
    let outputs_root = Path::new(&run.paths.outputs);
    run.artifacts
        .iter()
        .filter(|artifact| artifact.get("kind").and_then(serde_json::Value::as_str) == Some("output"))
        .filter_map(|artifact| artifact.get("path").and_then(serde_json::Value::as_str))
        .map(|path| relative_slash_path(outputs_root, Path::new(path)))
        .collect()
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
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let options = session.options;
    match options.grader {
        GraderMode::Script => grade_with_script(assertion, None, eval_case, ctx, options),
        GraderMode::Llm => grade_with_llm(assertion, &TargetDeclaration::Default, declarative, ctx, session),
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
                    weight: None,
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
            created_files: &ctx.created_files,
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
            weight: declared.weight,
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
            weight: declared.weight,
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
            weight: declared.weight,
        },
        GraderOutcome::Deferred { criterion, target } => {
            let mut result = match options.grader {
                GraderMode::Llm | GraderMode::Auto => grade_with_llm(&criterion, &target, declarative, ctx, session)?,
                GraderMode::Script => grade_with_script(&criterion, None, eval_case, ctx, options)?,
                GraderMode::None => needs_llm_result(&criterion, "grader mode is none, so no judge was consulted"),
            };
            result.excluded = excluded;
            result.name = name;
            result.weight = declared.weight;
            result
        }
        GraderOutcome::Comparison {
            criterion,
            target,
            reference,
        } => {
            let mut result = match options.grader {
                GraderMode::Llm | GraderMode::Auto => grade_against_baseline(
                    &assertion,
                    &criterion,
                    &target,
                    &reference,
                    eval_case,
                    declarative,
                    ctx,
                    session,
                )?,
                GraderMode::Script => grade_with_script(&criterion, Some(&reference), eval_case, ctx, options)?,
                GraderMode::None => needs_llm_result(&assertion, "grader mode is none, so no judge was consulted"),
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

/// Turn one logged `expect` mismatch into an ordinary failing assertion.
///
/// A violation never stops the mock from answering the call, so it never fails a run on
/// its own; this is the one place it turns into something grading already knows how to
/// tally, rather than a second reporting channel a caller has to learn.
fn mock_violation_result(violation: &super::mocks::MockViolation) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: format!(
            "mcp mock {}/{} honours its declared expectations",
            violation.server.as_str(),
            violation.tool.as_str()
        ),
        passed: false,
        evidence: format!(
            "`{}` was expected to satisfy `{}`, but the call received {}",
            violation.path.as_str(),
            violation.constraint,
            violation.received
        ),
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
        weight: None,
    }
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
        weight: None,
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
        weight: None,
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
        weight: None,
    }
}

/// `baseline` is `Some` only for a `baseline` grader, and carrying it here is what
/// keeps `--grader script` from quietly answering a different question: a script
/// handed only the criterion would grade the run on its own, which is not what a
/// comparison asked.
fn grade_with_script(
    assertion: &str,
    baseline: Option<&BaselineReference>,
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
    if let Some(baseline) = baseline {
        input["baseline"] = serde_json::json!({
            "reference": baseline.declared(),
            "content": baseline.text(),
        });
    }
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
        weight: None,
    })
}

#[derive(Debug, Deserialize)]
struct ScriptGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

fn grade_with_llm(
    assertion: &str,
    target: &TargetDeclaration,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    if !target.is_explicit() {
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
                weight: None,
            });
        }
    }

    let endpoint = session.endpoint()?;
    let payload = llm_grader_payload(assertion, &target.resolve(), declarative, ctx)?;
    let mut request = JudgeRequest::new(LLM_GRADER_SYSTEM_PROMPT, payload.text);
    if let Some(image) = payload.image {
        request = request.with_image(image);
    }

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
        weight: None,
    })
}

/// Which blind label the run's own output was given for one baseline comparison.
///
/// The judge is never told which side is the reference, because a judge that knows
/// which output is the incumbent is answering a different question from the one the
/// case wrote down. Which label the run gets is fixed by the case and the criterion,
/// the same deterministic rule `compare` already uses to debias its pairs, so
/// re-grading a report asks the judge the question it asked before rather than its
/// mirror image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BaselinePairing {
    run: BlindLabel,
}

impl BaselinePairing {
    /// Each side is held to the same share of the payload rather than letting a
    /// short output pass its unused share to the other, which is what
    /// `PayloadBudget` does for a list of artifacts. Rolling the leftover across
    /// would let the judge read one side whole and the other clipped, and "at least
    /// as good as" answered against a clipped reference compares two different
    /// things.
    const SIDE_BYTES: usize = PayloadBudget::TOTAL_BYTES / 2;

    fn for_criterion(eval_case_id: &str, criterion: &str) -> Self {
        let run = if shuffle_swap(&format!("{eval_case_id}:{criterion}")) {
            BlindLabel::B
        } else {
            BlindLabel::A
        };
        Self { run }
    }

    fn reference(self) -> BlindLabel {
        match self.run {
            BlindLabel::A => BlindLabel::B,
            BlindLabel::B => BlindLabel::A,
        }
    }

    fn outputs(self, run: &str, reference: &str, direction: TruncateDirection) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (self.run.as_str(), Self::fit(run, direction)),
            (self.reference().as_str(), Self::fit(reference, direction)),
        ])
    }

    fn fit(text: &str, direction: TruncateDirection) -> String {
        if text.len() <= Self::SIDE_BYTES {
            return text.to_string();
        }
        truncate(text, Self::SIDE_BYTES, direction)
    }

    /// A tie passes. "At least as good as" is the whole of what a baseline asks, so
    /// a run that matches the reference has met it; only a run the judge puts behind
    /// the reference has not.
    fn at_least_as_good(self, winner: ComparisonWinner) -> bool {
        match winner {
            ComparisonWinner::Tie => true,
            ComparisonWinner::A => self.run == BlindLabel::A,
            ComparisonWinner::B => self.run == BlindLabel::B,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BaselineJudgeResponse {
    better: String,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

const BASELINE_GRADER_SYSTEM_PROMPT: &str = "You are comparing two anonymous outputs, labeled A and B, \
     against one criterion. Say which one better satisfies the criterion, or answer tie when neither is \
     better than the other. Respond with JSON: {\"better\":\"A|B|tie\",\"evidence\":\"...\",\"rationale\":\"...\"}. \
     Quote both outputs in `evidence`. One of them is a reference and one is a new run; you are not told \
     which, and you must not guess.";

/// Grade a run against a reference output the suite already accepts.
///
/// A missing or unreadable target is settled here rather than sent to the judge:
/// a run that produced nothing is not at least as good as a reference that exists,
/// and there is nothing for a judge to read but the absence.
#[allow(clippy::too_many_arguments)]
fn grade_against_baseline(
    assertion: &str,
    criterion: &str,
    target: &GradeTarget,
    reference: &BaselineReference,
    eval_case: &EvalCase,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let run_output = match declarative.input(ctx).target_content(target) {
        TargetContent::Text(text) => text,
        TargetContent::Missing(reason) => return Ok(baseline_shortfall(assertion, reason)),
        TargetContent::Image { .. } => {
            return Ok(baseline_shortfall(
                assertion,
                format!("{target} is an image, and a baseline reference is compared as text"),
            ))
        }
    };

    // A blank reference is refused where it is read, and a blank run has to fall at the
    // same line. `final_text` reads a missing `final.md` as an empty string rather than
    // as a missing target, so without this a run that produced nothing reaches the judge
    // and passes on a tie against a reference it never answered.
    if run_output.trim().is_empty() {
        return Ok(baseline_shortfall(
            assertion,
            format!("{target} is empty, and nothing is not at least as good as a baseline"),
        ));
    }

    let direction = match target {
        GradeTarget::Transcript => TruncateDirection::Tail,
        _ => TruncateDirection::Head,
    };
    let pairing = BaselinePairing::for_criterion(eval_case.id.as_str(), criterion);
    let payload = serde_json::to_string(&serde_json::json!({
        "criterion": criterion,
        "outputs": pairing.outputs(&run_output, reference.text(), direction),
    }))?;

    let endpoint = session.endpoint()?;
    let request = JudgeRequest::new(BASELINE_GRADER_SYSTEM_PROMPT, payload);

    let votes = session.options.grader_votes;
    let mut opinions = Vec::with_capacity(votes.count() as usize);
    for _ in votes.ballots() {
        let reply = judge::judge(endpoint, &request, "--grader-model")?;
        let parsed: BaselineJudgeResponse = judge::parse_json_reply(&reply, "--grader-model")?;
        let winner = parse_winner(&parsed.better)?;
        opinions.push((pairing.at_least_as_good(winner), parsed));
    }

    let verdict = tally_opinions(opinions).ok_or_else(|| {
        EvalError::Validation(
            ValidationError::for_field("--grader-votes", "no judge opinion was taken for this comparison").into(),
        )
    })?;

    let passed = verdict.tally.majority_passed();
    let standing = if passed {
        "is at least as good as"
    } else {
        "falls short of"
    };
    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed,
        evidence: format!(
            "the run {standing} baseline '{}': {}",
            reference.declared(),
            verdict.opinion.evidence
        ),
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
        weight: None,
    })
}

fn baseline_shortfall(assertion: &str, evidence: impl Into<String>) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.into(),
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
        weight: None,
    }
}

const LLM_GRADER_SYSTEM_PROMPT: &str = "You are grading one assertion about the output of a single agent run. \
     Decide only whether the assertion holds for the material you are given. \
     Quote the material in `evidence`; do not restate the assertion as its own evidence. \
     Respond with JSON: {\"passed\": true|false, \"evidence\": \"...\", \"rationale\": \"...\"}.";

/// Total bytes of content one judge request may carry, across every artifact
/// placed into it.
///
/// Previously each artifact (the final text, and separately every output file)
/// got its own 8,000-byte excerpt with no cap on how many artifacts a run could
/// contribute, so a run with many output files could inflate one assertion into
/// an unbounded request. The cap now applies once, to the request as a whole.
#[derive(Debug, Clone, Copy)]
struct PayloadBudget {
    remaining_bytes: usize,
    remaining_slots: usize,
}

impl PayloadBudget {
    const TOTAL_BYTES: usize = 8_000;

    fn new(slots: usize) -> Self {
        Self {
            remaining_bytes: Self::TOTAL_BYTES,
            remaining_slots: slots.max(1),
        }
    }

    /// Each remaining artifact claims an equal share of what is left, so one
    /// large early artifact does not starve every artifact placed after it,
    /// and a share an artifact does not need rolls forward to the rest.
    fn place(&mut self, text: &str, direction: TruncateDirection) -> Placement {
        let share = self.remaining_bytes / self.remaining_slots.max(1);
        self.remaining_slots = self.remaining_slots.saturating_sub(1);

        if share == 0 {
            return Placement::Omitted;
        }
        if text.len() <= share {
            self.remaining_bytes -= text.len();
            return Placement::Whole(text.to_string());
        }
        self.remaining_bytes -= share;
        Placement::Truncated(truncate(text, share, direction))
    }
}

enum Placement {
    Whole(String),
    Truncated(String),
    Omitted,
}

#[derive(Debug, Clone, Copy)]
enum TruncateDirection {
    Head,
    Tail,
}

fn truncate(text: &str, cap: usize, direction: TruncateDirection) -> String {
    match direction {
        TruncateDirection::Head => {
            let mut end = cap;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n[truncated after {end} bytes]", &text[..end])
        }
        // A transcript's interesting part is usually its end (the final tool
        // calls and the answer the run converged on), not its start, so the
        // judge keeps the tail rather than the head.
        TruncateDirection::Tail => truncate_transcript_tail(text, cap),
    }
}

fn truncate_bytes_from_tail(text: &str, cap: usize) -> String {
    let mut start = text.len().saturating_sub(cap);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let shown = text.len() - start;
    format!(
        "[showing the last {shown} bytes; earlier content truncated]\n{}",
        &text[start..]
    )
}

/// A flat byte cut lands wherever the budget runs out, which can fall inside
/// the one message a criterion is about and gives no sign that anything was
/// removed. A transcript is line-delimited NDJSON, one message per line, so
/// the cut can instead fall between messages: this keeps whole messages from
/// the end backward until the budget is spent, keeps the first message too
/// when room remains, and names what sat between them rather than
/// discarding it silently.
fn truncate_transcript_tail(text: &str, cap: usize) -> String {
    let messages: Vec<&str> = text.split('\n').filter(|line| !line.is_empty()).collect();
    if messages.len() < 2 {
        return truncate_bytes_from_tail(text, cap);
    }

    let mut used = 0usize;
    let mut first_kept = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        // The join puts a newline *between* messages, so the last one kept carries
        // none of its own. Charging it one spends a byte the output never writes,
        // and a transcript over the cap by nothing more than that loses a whole
        // message to pay for it.
        let joined = match index == messages.len() - 1 {
            true => message.len(),
            false => message.len() + 1,
        };
        if used + joined > cap {
            break;
        }
        used += joined;
        first_kept = index;
    }

    if first_kept == messages.len() {
        return truncate_bytes_from_tail(messages[messages.len() - 1], cap);
    }
    if first_kept == 0 {
        return messages.join("\n");
    }

    let tail = messages[first_kept..].join("\n");
    let first = messages[0];
    let remaining = cap.saturating_sub(used);

    // Strictly less: what is left has to cover the newline that joins the first
    // message to what follows, not only the message itself.
    if first.len() < remaining {
        let hidden = first_kept - 1;
        if hidden == 0 {
            format!("{first}\n{tail}")
        } else {
            format!("{first}\n[{hidden} message(s) omitted from the middle of the transcript]\n{tail}")
        }
    } else {
        format!("[{first_kept} message(s) omitted from the start of the transcript]\n{tail}")
    }
}

/// What a judge request needs beyond a system prompt: the text payload, and,
/// when a target resolves to a picture, the attachment that carries it.
///
/// A bare `String` cannot also carry an optional image without either
/// smuggling it into the text (which is exactly the shape mismatch a vision
/// content part exists to avoid) or falling back to a tuple that leaves the
/// pairing anonymous at every call site.
struct JudgePayload {
    text: String,
    image: Option<judge::ImageAttachment>,
}

impl JudgePayload {
    fn text_only(text: String) -> Self {
        Self { text, image: None }
    }
}

fn llm_grader_payload(
    assertion: &str,
    target: &GradeTarget,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
) -> Result<JudgePayload> {
    match target {
        GradeTarget::FinalText => {
            legacy_structured_payload(assertion, declarative, ctx, OutputScope::TopLevel).map(JudgePayload::text_only)
        }
        GradeTarget::AnyOutput => {
            legacy_structured_payload(assertion, declarative, ctx, OutputScope::Nested).map(JudgePayload::text_only)
        }
        GradeTarget::Transcript | GradeTarget::File(_) | GradeTarget::CreatedFiles | GradeTarget::Files(_) => {
            single_target_payload(assertion, target, declarative, ctx)
        }
    }
}

/// How far under `outputs/` a structured payload looks for files to hand the
/// judge alongside the final text.
///
/// `final_text` (declared or defaulted) keeps the pre-target behavior of
/// looking only at what sits directly in `outputs/`, unchanged so a suite
/// written before targets existed still grades the same way. `any_output`
/// means the same thing here that it means to the mechanical grader, which
/// walks every file the run produced, nested directories included.
enum OutputScope {
    TopLevel,
    Nested,
}

fn collect_output_files(outputs_dir: &Path, scope: OutputScope) -> Vec<(String, String)> {
    let mut output_files = Vec::new();
    match scope {
        OutputScope::TopLevel => {
            if let Ok(entries) = std::fs::read_dir(outputs_dir) {
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
                        output_files.push((name, text));
                    }
                }
            }
        }
        OutputScope::Nested => collect_output_files_nested(outputs_dir, outputs_dir, &mut output_files),
    }
    output_files
}

fn collect_output_files_nested(root: &Path, dir: &Path, output_files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_output_files_nested(root, &path, output_files);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let name = relative_slash_path(root, &path);
        if name == FINAL_MD {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            output_files.push((name, text));
        }
    }
}

/// A path relative to `root`, rendered with `/` separators regardless of
/// platform, so a nested output's key in the judge payload matches what an
/// author would write in an assertion.
fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The payload shape used before the judge could be aimed at anything but the
/// final text plus the run's output files. Kept byte-for-byte for a suite that
/// does not declare a `target`.
fn legacy_structured_payload(
    assertion: &str,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    scope: OutputScope,
) -> Result<String> {
    let mut output_files = collect_output_files(&ctx.outputs_dir, scope);
    output_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut budget = PayloadBudget::new(1 + output_files.len());
    let mut omitted = 0usize;

    let final_text = match budget.place(&declarative.final_text, TruncateDirection::Head) {
        Placement::Whole(text) | Placement::Truncated(text) => text,
        Placement::Omitted => {
            omitted += 1;
            String::new()
        }
    };

    let mut outputs = serde_json::Map::new();
    for (name, text) in output_files {
        match budget.place(&text, TruncateDirection::Head) {
            Placement::Whole(text) | Placement::Truncated(text) => {
                outputs.insert(name, serde_json::Value::String(text));
            }
            Placement::Omitted => omitted += 1,
        }
    }

    let mut payload = serde_json::json!({
        "assertion": assertion,
        "final_text": final_text,
        "outputs": outputs,
    });
    if omitted > 0 {
        payload["artifacts_omitted"] =
            serde_json::Value::String(format!("{omitted} artifact(s) omitted: judge payload budget exhausted"));
    }

    Ok(serde_json::to_string(&payload)?)
}

/// The payload shape for a judge aimed at one specific target. Self-describing
/// (it names the target) since, unlike the legacy shape, the judge is not
/// implicitly looking at "the run's output" but at whatever was declared.
fn single_target_payload(
    assertion: &str,
    target: &GradeTarget,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
) -> Result<JudgePayload> {
    let content = declarative.input(ctx).target_content(target);
    let direction = match target {
        GradeTarget::Transcript => TruncateDirection::Tail,
        _ => TruncateDirection::Head,
    };

    let mut payload = serde_json::json!({
        "assertion": assertion,
        "target": target.to_string(),
    });
    let mut image = None;

    match content {
        TargetContent::Text(text) => match PayloadBudget::new(1).place(&text, direction) {
            Placement::Whole(text) | Placement::Truncated(text) => {
                payload["content"] = serde_json::Value::String(text);
            }
            Placement::Omitted => {
                payload["artifacts_omitted"] =
                    serde_json::Value::String("1 artifact(s) omitted: judge payload budget exhausted".to_string());
            }
        },
        TargetContent::Image { media_type, bytes } => {
            payload["content"] = serde_json::Value::String("image attached separately".to_string());
            image = Some(judge::ImageAttachment::new(media_type, &bytes));
        }
        TargetContent::Missing(reason) => {
            payload["missing_reason"] = serde_json::Value::String(reason);
        }
    }

    Ok(JudgePayload {
        text: serde_json::to_string(&payload)?,
        image,
    })
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
    use crate::agentskills::evals::RelativeSkillPath;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, RunMetrics, RunNotStarted, RunPaths,
        ScenarioKind, WriteReportOptions,
    };
    use crate::agentskills::runner::capabilities::HarnessControl;
    use crate::agentskills::runner::Runner;
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

    const ALL_UNSUPPORTED_SUITE: &str = r#"{
        "schema_version": 3,
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used", "arm": "both"}
                ]
            }
        ]
    }"#;

    fn unobservable_report_dir(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        unobservable_report_dir_with_suite(temp, UNOBSERVABLE_SUITE)
    }

    fn unobservable_report_dir_with_suite(temp: &tempfile::TempDir, suite_json: &str) -> (PathBuf, PathBuf) {
        let skill_dir = temp.path().join("demo-skill");
        fs::create_dir_all(skill_dir.join("evals")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        )
        .unwrap();
        fs::write(skill_dir.join("evals/evals.json"), suite_json).unwrap();

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

    #[test]
    fn a_case_scored_only_by_unsupported_checks_has_no_case_score() {
        let temp = tempdir().unwrap();
        let (report_dir, _run_dir) = unobservable_report_dir_with_suite(&temp, ALL_UNSUPPORTED_SUITE);

        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        let document: ReportDocument =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        let run = &document.runs[0];
        assert_eq!(
            run.case_score, None,
            "a case scored on nothing must not read as a score of zero"
        );
    }

    #[test]
    fn a_case_scored_and_failing_every_check_reports_a_score_of_zero_not_no_score() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, TRIGGERING_ONLY_SUITE);

        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        let document: ReportDocument =
            serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        for run in &document.runs {
            let workspace = report_dir.join(&run.paths.workspace);
            let run_dir = workspace.parent().unwrap();
            let grading: GradingFile =
                serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
            let expected = GradingCounts::tally(&grading.assertion_results).pass_rate();
            assert_eq!(run.case_score, expected, "{:?}", run.scenario_id);
        }

        let with_skill = document
            .runs
            .iter()
            .find(|r| r.scenario_id == ScenarioKind::WithSkill)
            .unwrap();
        let without_skill = document
            .runs
            .iter()
            .find(|r| r.scenario_id == ScenarioKind::WithoutSkill)
            .unwrap();
        assert_eq!(with_skill.case_score, Some(1.0));
        assert_eq!(
            without_skill.case_score,
            Some(0.0),
            "a case that scored and failed everything is a zero, not a missing score"
        );
    }

    #[test]
    fn a_run_the_harness_never_started_is_withheld_from_grading() {
        let temp = tempdir().unwrap();
        let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

        let report_path = report_dir.join("report.json");
        let mut document: ReportDocument = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
        document.runs[0].not_started(RunNotStarted::ControlUnsupported {
            control: HarnessControl::ConversationSeeding,
            runner: Runner::ClaudeCode,
        });
        let abandoned = report_dir.join(&document.runs[0].paths.workspace);
        fs::remove_dir_all(&abandoned).unwrap();
        document.runs[1].status = "completed".to_string();
        fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            report.runs_graded, 1,
            "a run that never reached the harness has no workspace to grade"
        );
        assert_eq!(report.run_statuses.skipped, 1);
        assert_eq!(report.run_statuses.completed, 1);
        assert_eq!(
            report.failed, 0,
            "a run trg declined to start must not be reported as a regression"
        );
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
            weight: None,
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
                weight: None,
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

    fn weighted_result(passed: bool, unsupported: bool, weight: Option<GraderWeight>) -> AssertionGradeResult {
        AssertionGradeResult {
            name: None,
            assertion: "the output includes a summary".to_string(),
            passed,
            evidence: "found the summary section".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: unsupported.then(|| "runner cannot observe this".to_string()),
            excluded: None,
            ungraded: None,
            votes: None,
            weight,
        }
    }

    /// A grader that leaves weight undeclared inside an otherwise-weighted case
    /// still has to count for exactly as much as it always did: one full vote,
    /// same as a declared weight of 1.0 would.
    #[test]
    fn an_undeclared_weight_counts_as_one_full_vote_alongside_a_declared_weight() {
        let results = vec![
            weighted_result(true, false, Some(GraderWeight::parse(3.0).unwrap())),
            weighted_result(false, false, None),
        ];

        assert_eq!(case_score(&results), Some(0.75));
    }

    /// A case where every grader left weight undeclared has to take the exact
    /// pre-weighting code path, not merely a formula that happens to agree with
    /// it, so a suite that never opts in cannot see its score move.
    #[test]
    fn case_score_falls_back_to_the_pre_weighting_pass_rate_when_nothing_declares_a_weight() {
        let results = vec![
            weighted_result(true, false, None),
            weighted_result(true, false, None),
            weighted_result(true, false, None),
            weighted_result(false, false, None),
            weighted_result(true, true, None),
        ];

        assert_eq!(case_score(&results), Some(0.75));
        assert_eq!(case_score(&results), GradingCounts::tally(&results).pass_rate());
    }

    /// An undeclared weight must not be constructible from a build that never
    /// wrote one, so the field cannot appear where nothing asked for it.
    #[test]
    fn an_unweighted_result_is_absent_from_the_serialized_json() {
        let result = weighted_result(true, false, None);
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("\"weight\""));
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
    fn a_declared_baseline_grader_needs_a_judge_under_auto() {
        let suite = suite_from(
            r#"{
                "schema_version": 3,
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "baseline", "reference": "golden.md", "criterion": "is as complete"}]
                    }
                ]
            }"#,
        );
        let options = GradeOptions {
            grader: GraderMode::Auto,
            ..GradeOptions::default()
        };

        assert!(
            suite_needs_a_judge(&options, &suite),
            "a comparison is a judgement call, so the credential is demanded before the first run is graded"
        );
        let err = GradeSession::open(&options, &suite).unwrap_err();
        assert!(err.to_string().contains("--grader-model"), "{err}");
    }

    #[test]
    fn a_baseline_comparison_is_put_to_the_judge_blind_and_the_same_way_every_time() {
        let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
        let outputs = pairing.outputs("what the run wrote", "the reference", TruncateDirection::Head);

        assert_eq!(outputs.len(), 2, "the judge is shown both sides and nothing else");
        assert_eq!(
            outputs.keys().copied().collect::<Vec<_>>(),
            vec!["A", "B"],
            "the labels carry no hint of which side is which"
        );
        assert_eq!(outputs[pairing.run.as_str()], "what the run wrote");
        assert_eq!(outputs[pairing.reference().as_str()], "the reference");
        assert_ne!(pairing.run, pairing.reference());

        assert_eq!(
            pairing,
            BaselinePairing::for_criterion("case-a", "is as complete"),
            "re-grading a report must ask the judge the question it asked before, not its mirror"
        );
    }

    #[test]
    fn which_side_a_baseline_puts_first_is_decided_by_the_case_and_the_criterion() {
        let pairings: Vec<BlindLabel> = [
            "is as complete",
            "is as well organized",
            "is as precise",
            "reads as well",
        ]
        .into_iter()
        .map(|criterion| BaselinePairing::for_criterion("case-a", criterion).run)
        .collect();

        assert!(
            pairings.contains(&BlindLabel::A) && pairings.contains(&BlindLabel::B),
            "a fixed position would leave every comparison carrying the judge's position bias, got {pairings:?}"
        );
    }

    #[test]
    fn a_run_the_judge_cannot_separate_from_its_baseline_has_met_it() {
        for run in [BlindLabel::A, BlindLabel::B] {
            let pairing = BaselinePairing { run };
            let reference = pairing.reference();

            assert!(
                pairing.at_least_as_good(ComparisonWinner::Tie),
                "at least as good is met by a tie, or no baseline could ever be held"
            );
            assert!(pairing.at_least_as_good(winner_of(run)));
            assert!(!pairing.at_least_as_good(winner_of(reference)));
        }
    }

    fn winner_of(label: BlindLabel) -> ComparisonWinner {
        match label {
            BlindLabel::A => ComparisonWinner::A,
            BlindLabel::B => ComparisonWinner::B,
        }
    }

    #[test]
    fn both_sides_of_a_baseline_comparison_are_clipped_to_the_same_size() {
        let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
        let run = "r".repeat(BaselinePairing::SIDE_BYTES * 2);
        let reference = "f".repeat(BaselinePairing::SIDE_BYTES * 4);

        let outputs = pairing.outputs(&run, &reference, TruncateDirection::Head);

        let shown = |label: BlindLabel| outputs[label.as_str()].len();
        assert_eq!(
            shown(pairing.run),
            shown(pairing.reference()),
            "reading one side whole and the other clipped compares two different things"
        );
        assert!(shown(pairing.run) < run.len());
    }

    #[test]
    fn a_short_baseline_reference_is_never_clipped_to_make_room_for_a_long_run() {
        let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
        let reference = "the whole reference answer";

        let outputs = pairing.outputs(
            &"r".repeat(BaselinePairing::SIDE_BYTES * 3),
            reference,
            TruncateDirection::Head,
        );

        assert_eq!(outputs[pairing.reference().as_str()], reference);
    }

    fn run_record_with_artifacts(artifacts: Vec<serde_json::Value>) -> RunRecord {
        RunRecord {
            id: "run-001".to_string(),
            eval_case_id: "case-a".to_string(),
            eval_slug: "case-a".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            iteration: 1,
            model_config_id: "ci-default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            failure_kind: None,
            runner_invocations: 1,
            status: "completed".to_string(),
            paths: RunPaths {
                workspace: "runs/run-001/workspace".to_string(),
                outputs: "runs/run-001/workspace/outputs".to_string(),
            },
            mirror_path: "iteration-1/eval-case-a/with-skill/".to_string(),
            artifacts,
            metrics: RunMetrics {
                duration_ms: Some(1),
                exit_code: Some(0),
                total_tokens: None,
                input_tokens: None,
                output_tokens: None,
                cost_usd: None,
            },
            cache: None,
            skill_integrity: None,
            read_only_fixture_violations: Vec::new(),
            warnings: Vec::new(),
            mock_violations: Vec::new(),
            case_score: None,
        }
    }

    #[test]
    fn created_files_from_artifacts_strips_a_mismatched_outputs_prefix() {
        let mut run = run_record_with_artifacts(vec![serde_json::json!({
            "kind": "output",
            "path": "runs/run-001/workspace/outputs/report.md",
            "size_bytes": 4,
            "sha256": "abc",
        })]);
        run.paths.outputs = "runs/run-001/workspace/outputs/".to_string();

        let created = created_files_from_artifacts(&run);

        assert_eq!(
            created,
            vec!["report.md".to_string()],
            "a path-aware strip must not be defeated by a trailing separator on the outputs root"
        );
    }

    #[test]
    fn created_files_from_artifacts_strips_the_outputs_prefix_and_ignores_other_kinds() {
        let run = run_record_with_artifacts(vec![
            serde_json::json!({
                "kind": "output",
                "path": "runs/run-001/workspace/outputs/report.md",
                "size_bytes": 4,
                "sha256": "abc",
            }),
            serde_json::json!({
                "kind": "output",
                "path": "runs/run-001/workspace/outputs/sub/data.json",
                "size_bytes": 2,
                "sha256": "def",
            }),
            serde_json::json!({
                "kind": "transcript",
                "path": "runs/run-001/transcript.jsonl",
            }),
        ]);

        let created = created_files_from_artifacts(&run);

        assert_eq!(created, vec!["report.md".to_string(), "sub/data.json".to_string()]);
    }

    fn declarative_context(final_text: &str, raw_transcript: &str) -> DeclarativeContext {
        DeclarativeContext {
            final_text: final_text.to_string(),
            raw_transcript: raw_transcript.to_string(),
            transcript: None,
        }
    }

    #[test]
    fn the_default_target_reproduces_the_pre_target_payload_shape() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.txt"), "an output file").unwrap();
        let declarative = declarative_context("the final answer", "irrelevant to this target");

        let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "assertion": "the assertion",
                "final_text": "the final answer",
                "outputs": {"out.txt": "an output file"},
            })
        );
    }

    #[test]
    fn any_output_target_uses_the_same_structured_shape_as_the_default() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.txt"), "an output file").unwrap();
        let declarative = declarative_context("the final answer", "irrelevant to this target");

        let payload = llm_grader_payload("the assertion", &GradeTarget::AnyOutput, &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "assertion": "the assertion",
                "final_text": "the final answer",
                "outputs": {"out.txt": "an output file"},
            })
        );
    }

    #[test]
    fn any_output_walks_nested_output_files_but_the_default_final_text_target_does_not() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.outputs_dir.join("out.txt"), "top level").unwrap();
        fs::create_dir_all(ctx.outputs_dir.join("sub")).unwrap();
        fs::write(ctx.outputs_dir.join("sub").join("nested.txt"), "buried").unwrap();
        let declarative = declarative_context("the final answer", "irrelevant to this target");

        let any_output = llm_grader_payload("the assertion", &GradeTarget::AnyOutput, &declarative, &ctx)
            .unwrap()
            .text;
        let any_output: serde_json::Value = serde_json::from_str(&any_output).unwrap();
        assert_eq!(
            any_output["outputs"],
            serde_json::json!({"out.txt": "top level", "sub/nested.txt": "buried"}),
            "any_output must see everything the mechanical grader's collect_text would see"
        );

        let final_text = llm_grader_payload("the assertion", &GradeTarget::FinalText, &declarative, &ctx)
            .unwrap()
            .text;
        let final_text: serde_json::Value = serde_json::from_str(&final_text).unwrap();
        assert_eq!(
            final_text["outputs"],
            serde_json::json!({"out.txt": "top level"}),
            "final_text keeps the pre-target, top-level-only scope"
        );
    }

    #[test]
    fn transcript_target_names_itself_and_carries_the_transcript() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let declarative = declarative_context("the final answer", "the agent read the config then wrote it back");

        let payload = llm_grader_payload("the assertion", &GradeTarget::Transcript, &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "assertion": "the assertion",
                "target": "transcript",
                "content": "the agent read the config then wrote it back",
            })
        );
    }

    #[test]
    fn file_target_reports_missing_reason_when_the_file_does_not_exist() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let declarative = declarative_context("the final answer", "");

        let path: RelativeSkillPath = serde_json::from_value(serde_json::json!("missing.json")).unwrap();
        let target = GradeTarget::File(path);
        let payload = llm_grader_payload("the assertion", &target, &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(value["assertion"], "the assertion");
        assert_eq!(value["target"], "file 'missing.json'");
        assert!(value.get("content").is_none());
        assert!(value["missing_reason"].as_str().unwrap().contains("missing.json"));
    }

    #[test]
    fn a_file_target_pointing_at_a_picture_attaches_it_instead_of_failing_to_read_it_as_text() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        fs::write(
            ctx.outputs_dir.join("screenshot.png"),
            [0x89, 0x50, 0x4E, 0x47, 1, 2, 3],
        )
        .unwrap();
        let declarative = declarative_context("the final answer", "");

        let path: RelativeSkillPath = serde_json::from_value(serde_json::json!("screenshot.png")).unwrap();
        let target = GradeTarget::File(path);
        let payload = llm_grader_payload("the assertion", &target, &declarative, &ctx).unwrap();

        assert!(
            payload.image.is_some(),
            "a picture target must carry an image attachment for the judge to see"
        );
        let value: serde_json::Value = serde_json::from_str(&payload.text).unwrap();
        assert!(
            value.get("missing_reason").is_none(),
            "an image target must not be reported as unreadable: {value}"
        );
    }

    #[test]
    fn created_files_target_lists_what_the_run_produced() {
        let tmp = tempdir().unwrap();
        let mut ctx = ctx_with_outputs(tmp.path());
        ctx.created_files = vec!["report.md".to_string(), "sub/data.json".to_string()];
        let declarative = declarative_context("the final answer", "");

        let payload = llm_grader_payload("the assertion", &GradeTarget::CreatedFiles, &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "assertion": "the assertion",
                "target": "created files",
                "content": "report.md\nsub/data.json",
            })
        );
    }

    #[test]
    fn payload_budget_divides_across_every_artifact_and_flags_what_it_drops() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        // Far larger than the whole request budget, so this artifact alone
        // must be truncated no matter how the remaining budget is split.
        let oversized = "a".repeat(PayloadBudget::TOTAL_BYTES * 5);
        fs::write(ctx.outputs_dir.join("a.txt"), &oversized).unwrap();
        fs::write(ctx.outputs_dir.join("b.txt"), "b").unwrap();
        let declarative = declarative_context("c", "");

        let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let a = value["outputs"]["a.txt"].as_str().unwrap();
        assert!(
            a.len() < oversized.len(),
            "an oversized artifact must be truncated, got {} bytes back unchanged",
            a.len()
        );
        assert!(a.contains("[truncated after"), "truncation must stay visible: {a}");
        // What b.txt received is whatever a.txt's truncation left unused,
        // rolled forward, so its exact size depends on that share; only its
        // content need survive untouched, since one byte never needs truncating.
        assert_eq!(value["outputs"]["b.txt"], "b");
        assert_eq!(value["final_text"], "c");
        assert!(value.get("artifacts_omitted").is_none());
    }

    #[test]
    fn an_artifact_needing_less_than_its_share_leaves_the_rest_for_the_one_after_it() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        // With 3 artifacts, final_text's initial share is TOTAL_BYTES / 3, but
        // final_text ("c") needs almost none of it. a.txt is bigger than that
        // fixed share, so it is only placed whole if the unused share rolled
        // forward into a.txt's own share instead of being wasted.
        let fixed_share = PayloadBudget::TOTAL_BYTES / 3;
        let a_len = fixed_share + 500;
        fs::write(ctx.outputs_dir.join("a.txt"), "a".repeat(a_len)).unwrap();
        fs::write(ctx.outputs_dir.join("b.txt"), "b").unwrap();
        let declarative = declarative_context("c", "");

        let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let a = value["outputs"]["a.txt"].as_str().unwrap();
        assert_eq!(
            a.len(),
            a_len,
            "a.txt fits within the rolled-forward share and must not be truncated: {a}"
        );
        assert!(!a.contains("[truncated after"));
    }

    #[test]
    fn payload_budget_answers_a_placement_it_was_never_told_to_expect() {
        let mut budget = PayloadBudget::new(1);

        assert!(matches!(
            budget.place("first", TruncateDirection::Head),
            Placement::Whole(_)
        ));
        assert!(matches!(
            budget.place("second", TruncateDirection::Head),
            Placement::Whole(_)
        ));
    }

    #[test]
    fn payload_budget_omits_rather_than_silently_dropping_when_slots_outnumber_bytes() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        for i in 0..(PayloadBudget::TOTAL_BYTES + 10) {
            fs::write(ctx.outputs_dir.join(format!("file-{i}.txt")), "x").unwrap();
        }
        let declarative = declarative_context("final", "");

        let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let note = value["artifacts_omitted"]
            .as_str()
            .expect("omissions must be reported, not silent");
        assert!(note.contains("omitted"));
        // The budget's per-slot share is a non-decreasing ratio, so every omission
        // sits in a prefix of the placement order: final_text plus the first ten
        // output files (the ones seen while remaining_slots still exceeds 8000)
        // before the ratio recovers to one byte per slot for the rest. A count
        // that drifts from 11 means an artifact was silently dropped somewhere
        // in the loop without being tallied.
        assert!(
            note.contains("11 artifact"),
            "expected exactly 11 omissions (final_text plus the first ten output files): {note}"
        );
    }

    #[test]
    fn transcript_truncation_keeps_the_tail_not_the_head() {
        let head = "A".repeat(50);
        let tail = "B".repeat(50);
        let long_transcript = format!("{head}{tail}");

        let truncated = truncate(&long_transcript, 50, TruncateDirection::Tail);

        assert!(!truncated.contains('A'), "the head must be dropped: {truncated}");
        assert!(truncated.contains(&tail));
    }

    #[test]
    fn transcript_truncation_keeps_the_first_and_last_message_and_names_what_it_drops() {
        let first = "FIRST_MESSAGE".to_string();
        let middle = "MIDDLE_MESSAGE_".repeat(400);
        let last = "LAST_MESSAGE".to_string();
        let long_transcript = format!("{first}\n{middle}\n{last}");

        let truncated = truncate(&long_transcript, 100, TruncateDirection::Tail);

        assert!(
            truncated.contains(&first),
            "the first message must survive so the judge sees how the run began: {truncated}"
        );
        assert!(
            truncated.contains(&last),
            "the last message must survive so the judge sees how the run ended: {truncated}"
        );
        assert!(
            !truncated.contains(&middle),
            "the dropped middle message must not appear: {truncated}"
        );
        assert!(
            truncated.contains("omitted"),
            "the elision must be visible rather than silent: {truncated}"
        );
    }

    /// A transcript is written a line at a time, so it ends with a newline that the
    /// join never writes back. Counted against the budget anyway, a transcript that is
    /// over the cap by that byte and nothing else pays for it with a whole message.
    #[test]
    fn a_transcript_over_the_cap_by_only_its_trailing_newline_keeps_every_message() {
        let messages = ["FIRST_MESSAGE", "MIDDLE_MESSAGE", "LAST_MESSAGE"];
        let joined = messages.join("\n");
        let transcript = format!("{joined}\n");
        let cap = joined.len();
        assert!(transcript.len() > cap, "the transcript is over the cap by its newline");

        let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

        assert_eq!(
            truncated, joined,
            "nothing was over budget, so nothing may be dropped or annotated"
        );
    }

    /// What is left over is whatever the tail did not spend, so an overstated tail
    /// understates it. A first message that fits the room actually left is dropped and
    /// reported as omitted.
    #[test]
    fn a_first_message_that_fits_the_leftover_budget_exactly_is_kept() {
        let first = "FIRST";
        let last = "LAST";
        let transcript = format!("{first}\n{}\n{last}", "M".repeat(100));
        let cap = first.len() + 1 + last.len();

        let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

        assert!(
            truncated.contains(first),
            "the first message fits the room left, so the judge must still see how the run began: {truncated}"
        );
        assert!(truncated.contains(last), "the tail must survive: {truncated}");
        assert!(
            !truncated.contains("from the start"),
            "the first message was kept, so nothing was omitted from the start: {truncated}"
        );
    }

    /// The other side of the same boundary: the newline the first message is joined by
    /// is a real byte, so a first message that would fit only without it does not fit,
    /// and keeping it would spend more of the judge's budget than the cap allows.
    #[test]
    fn a_first_message_one_byte_past_the_leftover_budget_is_not_kept() {
        let first = "FIRSTX";
        let last = "LAST";
        let transcript = format!("{first}\n{}\n{last}", "M".repeat(100));
        let cap = first.len() + last.len();

        let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

        assert!(
            !truncated.contains(first),
            "the first message is one byte past what is left, so it cannot be kept: {truncated}"
        );
        assert!(
            truncated.contains("from the start"),
            "dropping the opening of the run must be said out loud: {truncated}"
        );
    }

    #[test]
    fn an_oversized_transcript_reaches_the_judge_tail_truncated_via_the_full_payload() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let head = "A".repeat(50);
        let tail = "B".repeat(PayloadBudget::TOTAL_BYTES + 100);
        let declarative = declarative_context("final", &format!("{head}{tail}"));

        let payload = llm_grader_payload("the assertion", &GradeTarget::Transcript, &declarative, &ctx)
            .unwrap()
            .text;
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let content = value["content"].as_str().unwrap();

        assert!(
            !content.contains('A'),
            "the head must be dropped through the full payload path: {content}"
        );
    }

    #[test]
    fn final_text_truncation_keeps_the_head_not_the_tail() {
        let head = "A".repeat(50);
        let tail = "B".repeat(50);
        let long_text = format!("{head}{tail}");

        let truncated = truncate(&long_text, 50, TruncateDirection::Head);

        assert!(!truncated.contains('B'), "the tail must be dropped: {truncated}");
        assert!(truncated.contains(&head));
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
    fn an_explicit_target_suppresses_the_mechanical_shortcut_even_when_the_criterion_parses_as_mechanical() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let declarative = declarative_context("done", "the agent reported that report.md exists");
        let options = GradeOptions::default();
        let session = GradeSession {
            options: &options,
            judge: None,
        };

        let mechanical_sounding = "report.md exists";

        let defaulted = grade_with_llm(
            mechanical_sounding,
            &TargetDeclaration::Default,
            &declarative,
            &ctx,
            &session,
        );
        assert!(
            defaulted.is_ok(),
            "a defaulted target should still take the mechanical shortcut without a judge: {defaulted:?}"
        );
        assert_eq!(defaulted.unwrap().grader.kind, GraderKind::Mechanical);

        let declared = grade_with_llm(
            mechanical_sounding,
            &TargetDeclaration::Named(GradeTarget::Transcript),
            &declarative,
            &ctx,
            &session,
        );
        let err = declared.expect_err("an explicit target must reach the judge instead of the mechanical shortcut");
        assert!(
            err.to_string().contains("no LLM judge was resolved"),
            "expected the judge lookup to fail, got: {err}"
        );
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
                weight: None,
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
                weight: None,
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
            weight: None,
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
            weight: None,
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
    fn a_run_with_no_mcp_servers_control_is_skipped_rather_than_graded() {
        let temp = tempdir().unwrap();
        let (report_dir, run_dir) = unobservable_report_dir(&temp);

        let report_path = report_dir.join("report.json");
        let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
        document["runs"][0]["status"] = serde_json::json!("skipped");
        document["runs"][0]["failure_kind"] = serde_json::json!(crate::agentskills::runner::FAILURE_KIND_UNSUPPORTED);
        fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.runs_graded, 0);
        assert_eq!(report.assertions_graded, 0);
        assert_eq!(report.passed, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(report.unsupported, 0);
        assert_eq!(report.run_statuses.skipped, 1);
        assert!(
            !run_dir.join("grading.json").is_file(),
            "a run that never started has nothing to write grading.json from"
        );
    }

    #[test]
    fn a_logged_mock_violation_becomes_a_failing_assertion() {
        let temp = tempdir().unwrap();
        let (report_dir, run_dir) = unobservable_report_dir(&temp);

        let report_path = report_dir.join("report.json");
        let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
        document["runs"][0]["mock_violations"] = serde_json::json!([{
            "server": "github",
            "tool": "create_issue",
            "path": "title",
            "constraint": "matches /^feat/",
            "received": "bug: oops",
        }]);
        fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

        let report = grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            report.assertions_graded, 3,
            "the two declared graders plus the logged violation"
        );
        assert_eq!(
            report.failed, 1,
            "the violation never passes, whatever the declared graders did"
        );

        let grading: GradingFile =
            serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
        let violation_result = grading
            .assertion_results
            .iter()
            .find(|result| result.assertion.contains("github/create_issue"))
            .expect("the violation surfaces as an ordinary assertion result");
        assert!(!violation_result.passed);
        assert!(violation_result.evidence.contains("bug: oops"));
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
            created_files: Vec::new(),
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
                weight: None,
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

        let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();
        assert!(result.passed);
        assert_eq!(result.grader.kind, GraderKind::Script);
        assert!(result.evidence.contains("script verified"));
    }

    fn baseline_case() -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "case",
            "prompt": "prompt long enough",
            "expected_output": "expected output",
            "assertions": ["custom assertion"],
        }))
        .unwrap()
    }

    fn reference_in(dir: &Path, text: &str) -> BaselineReference {
        let resolved = dir.join("golden.md");
        fs::create_dir_all(dir).unwrap();
        fs::write(&resolved, text).unwrap();
        BaselineReference::read(
            &serde_json::from_value(serde_json::json!("golden.md")).unwrap(),
            &resolved,
        )
        .unwrap()
    }

    /// A run that produced nothing is not at least as good as a reference that
    /// exists, and there is nothing for a judge to read but the absence. Billing one
    /// to be told so is the cost half of the same mistake.
    #[test]
    fn a_run_that_produced_nothing_falls_short_of_its_baseline_without_a_judge_being_asked() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
        let options = GradeOptions {
            grader: GraderMode::Llm,
            ..GradeOptions::default()
        };
        let session = GradeSession {
            options: &options,
            judge: None,
        };

        let result = grade_against_baseline(
            "final text is at least as good as baseline 'golden.md' on: is as complete",
            "is as complete",
            &GradeTarget::File(serde_json::from_value(serde_json::json!("missing.json")).unwrap()),
            &reference,
            &baseline_case(),
            &DeclarativeContext::load(&ctx),
            &ctx,
            &session,
        )
        .unwrap();

        assert!(!result.passed);
        assert_eq!(result.grader.kind, GraderKind::Declarative);
        assert!(result.grader.model.is_none(), "no judge answered this one");
    }

    #[test]
    fn a_run_whose_output_is_empty_falls_short_of_its_baseline_without_a_judge_being_asked() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_outputs(tmp.path());
        let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
        let options = GradeOptions {
            grader: GraderMode::Llm,
            ..GradeOptions::default()
        };
        let session = GradeSession {
            options: &options,
            judge: None,
        };

        let result = grade_against_baseline(
            "final text is at least as good as baseline 'golden.md' on: is as complete",
            "is as complete",
            &GradeTarget::FinalText,
            &reference,
            &baseline_case(),
            &DeclarativeContext::load(&ctx),
            &ctx,
            &session,
        )
        .unwrap();

        assert!(!result.passed, "nothing is not at least as good as a baseline");
        assert!(result.grader.model.is_none(), "no judge was billed for an empty run");
    }

    #[test]
    fn a_baseline_graded_by_script_hands_the_script_what_it_must_compare_against() {
        let tmp = tempdir().unwrap();
        let script = tmp.path().join("grader.sh");
        fs::write(
            &script,
            r#"#!/bin/bash
payload=$(cat)
case "$payload" in
  *"The reference answer."*) echo '{"passed": true, "evidence": "the reference reached the script"}' ;;
  *) echo '{"passed": false, "evidence": "the script was asked to compare against nothing"}' ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let ctx = ctx_with_outputs(tmp.path());
        let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_command: Some(script.to_string_lossy().into_owned()),
            ..GradeOptions::default()
        };

        let result = grade_with_script("is as complete", Some(&reference), &baseline_case(), &ctx, &options).unwrap();

        assert!(result.passed, "{}", result.evidence);
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

        let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();

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

        let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();

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
            let _ =
                done.send(grade_with_script("custom assertion", None, &eval_case, &ctx, &options).map(|r| r.passed));
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
            weight: None,
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
