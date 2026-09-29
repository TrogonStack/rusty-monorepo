//! Human review feedback artifacts for skill eval runs.
//!
//! Reviewers record findings in per-run `feedback.json` files. Each note should be
//! **specific and actionable** — cite what failed or could improve, where it
//! appears (artifact path, assertion, or transcript line), and what "good" looks
//! like. Vague notes such as "looks wrong" are not useful for iteration prompts.
//!
//! An empty `notes` array means the run was reviewed with no issues found; that
//! is an explicit signal, not missing data.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::evals::EvalError;
use super::grading::GradingFile;
use super::report::ScenarioKind;

pub const FEEDBACK_FILE_NAME: &str = "feedback.json";

#[derive(Error, Debug)]
pub enum FeedbackError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, FeedbackError>;

impl From<FeedbackError> for EvalError {
    fn from(value: FeedbackError) -> Self {
        match value {
            FeedbackError::Io(e) => EvalError::Io(e),
            FeedbackError::Json(e) => EvalError::Json(e),
            FeedbackError::Message(msg) => {
                EvalError::Validation(super::validation::ValidationError::for_field("feedback", msg).into())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSeverity {
    Info,
    Warning,
    Blocker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    Correctness,
    Style,
    Completeness,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackNote {
    pub severity: FeedbackSeverity,
    pub category: FeedbackCategory,
    pub text: String,
}

/// A human reviewer's verdict on one grader's call, for the grader-agreement report.
///
/// `pass`/`fail` rather than a bool, so a `feedback.json` a reviewer reads without the
/// schema in hand still says what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HumanVerdict {
    Pass,
    Fail,
}

impl HumanVerdict {
    pub fn passed(self) -> bool {
        matches!(self, HumanVerdict::Pass)
    }
}

impl std::fmt::Display for HumanVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HumanVerdict::Pass => write!(f, "pass"),
            HumanVerdict::Fail => write!(f, "fail"),
        }
    }
}

/// A human label on one assertion, keyed by its stable id (`<eval-case-id>:g<index>`) in
/// `FeedbackDocument::assertion_verdicts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssertionVerdict {
    pub verdict: HumanVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDocument {
    pub reviewer: String,
    pub reviewed_at: String,
    pub notes: Vec<FeedbackNote>,
    /// Keyed by the stable assertion id a grader's result carries in `grading.json`.
    /// A grader is only trustworthy where a human has actually looked, so this stays
    /// empty rather than inferred until a reviewer records a verdict.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assertion_verdicts: BTreeMap<String, AssertionVerdict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFeedbackEntry {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: ScenarioKind,
    pub source_path: String,
    pub feedback: FeedbackDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HumanFeedbackSummary {
    pub total_runs: usize,
    pub reviewed_runs: usize,
    pub pending_runs: usize,
    pub by_severity: FeedbackCountBySeverity,
    pub by_category: FeedbackCountByCategory,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackCountBySeverity {
    pub info: usize,
    pub warning: usize,
    pub blocker: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackCountByCategory {
    pub correctness: usize,
    pub style: usize,
    pub completeness: usize,
    pub other: usize,
}

/// Per-run feedback preserved for future iteration improvement prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImprovementFeedbackRecord {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: ScenarioKind,
    pub reviewer: String,
    pub reviewed_at: String,
    pub notes: Vec<FeedbackNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ReportRunsDocument {
    #[serde(default)]
    dimensions: ReportDimensionsRef,
    runs: Vec<ReportRunRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
struct ReportDimensionsRef {
    #[serde(default)]
    eval_cases: Vec<ReportEvalCaseRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ReportEvalCaseRef {
    id: String,
    #[serde(default)]
    assertion_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ReportRunRef {
    pub(crate) id: String,
    pub(crate) eval_case_id: String,
    pub(crate) scenario_id: ScenarioKind,
    pub(crate) paths: ReportRunPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ReportRunPaths {
    pub(crate) workspace: String,
}

/// A report's runs, together with the assertion ids each eval case declared, so a
/// caller can tell a graded assertion from one that was never scored without
/// re-reading `report.json` itself.
pub(crate) struct LoadedReport {
    pub(crate) runs: Vec<ReportRunRef>,
    pub(crate) assertion_ids_by_case: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FeedbackInitReport {
    pub created: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FeedbackValidateReport {
    pub validated: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FeedbackLabelReport {
    pub run_id: String,
    pub assertion_id: String,
    pub verdict: HumanVerdict,
    /// Whether this call replaced a verdict a prior `label` call had already recorded.
    pub replaced: bool,
}

pub fn resolve_reviewer(override_reviewer: Option<&str>) -> Result<String> {
    if let Some(reviewer) = override_reviewer {
        let trimmed = reviewer.trim();
        if trimmed.is_empty() {
            return Err(FeedbackError::Message(
                "reviewer must be a non-empty string when --reviewer is set".to_string(),
            ));
        }
        return Ok(trimmed.to_string());
    }

    git_user_email().ok_or_else(|| {
        FeedbackError::Message("could not resolve reviewer: set git user.email or pass --reviewer".to_string())
    })
}

fn run_dir_for_workspace(report_dir: &Path, workspace_rel: &str) -> PathBuf {
    let workspace = Path::new(workspace_rel);
    let run_rel = workspace
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(workspace_rel));
    report_dir.join(run_rel)
}

pub fn feedback_path_for_run(report_dir: &Path, workspace_rel: &str) -> PathBuf {
    run_dir_for_workspace(report_dir, workspace_rel).join(FEEDBACK_FILE_NAME)
}

/// Where a run's `grading.json` lives, by the same layout `feedback_path_for_run` uses.
pub fn grading_path_for_run(report_dir: &Path, workspace_rel: &str) -> PathBuf {
    run_dir_for_workspace(report_dir, workspace_rel).join("grading.json")
}

pub fn init_feedback(report_dir: &Path, reviewer_override: Option<&str>) -> Result<FeedbackInitReport> {
    let runs = load_report_runs(report_dir)?;
    let reviewer = resolve_reviewer(reviewer_override)?;
    let reviewed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let template = FeedbackDocument {
        reviewer,
        reviewed_at,
        notes: Vec::new(),
        assertion_verdicts: BTreeMap::new(),
    };

    let mut created = 0;
    let mut skipped = 0;

    for run in runs {
        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if feedback_path.is_file() {
            skipped += 1;
            continue;
        }

        if let Some(parent) = feedback_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&template)?;
        std::fs::write(&feedback_path, json)?;
        created += 1;
    }

    Ok(FeedbackInitReport { created, skipped })
}

pub fn list_runs_needing_review(report_dir: &Path) -> Result<Vec<String>> {
    let runs = load_report_runs(report_dir)?;
    let mut pending = Vec::new();

    for run in runs {
        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if !feedback_path.is_file() {
            pending.push(run.id);
        }
    }

    Ok(pending)
}

pub fn validate_feedback(report_dir: &Path) -> Result<FeedbackValidateReport> {
    let loaded = load_report(report_dir)?;
    let mut validated = 0;
    let mut errors = Vec::new();

    for run in &loaded.runs {
        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if !feedback_path.is_file() {
            continue;
        }

        match read_feedback_document(&feedback_path) {
            Ok(document) => {
                match validate_assertion_ids_are_graded(report_dir, run, &loaded.assertion_ids_by_case, &document) {
                    Ok(()) => validated += 1,
                    Err(message) => errors.push(format!("{} (run {}): {}", feedback_path.display(), run.id, message)),
                }
            }
            Err(error) => errors.push(format!("{} (run {}): {}", feedback_path.display(), run.id, error)),
        }
    }

    Ok(FeedbackValidateReport { validated, errors })
}

/// Records (or replaces) one human verdict on a graded assertion.
///
/// Idempotent by design: labelling the same `(run, assertion)` pair again replaces the
/// prior verdict rather than accumulating a history a reader would have to reconcile.
pub fn record_assertion_verdict(
    report_dir: &Path,
    run_id: &str,
    assertion_id: &str,
    verdict: HumanVerdict,
    rationale: Option<String>,
    reviewer_override: Option<&str>,
) -> Result<FeedbackLabelReport> {
    let loaded = load_report(report_dir)?;
    let run = loaded
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| FeedbackError::Message(format!("run '{run_id}' not found in report.json")))?;

    let graded = graded_assertion_ids(report_dir, run, &loaded.assertion_ids_by_case)?;
    if !graded.iter().any(|id| id == assertion_id) {
        return Err(FeedbackError::Message(format!(
            "assertion '{assertion_id}' is not a graded assertion for run '{run_id}'"
        )));
    }

    let rationale = match rationale {
        Some(text) if !text.trim().is_empty() => Some(text),
        Some(_) => {
            return Err(FeedbackError::Message(
                "rationale must be a non-empty string when set".to_string(),
            ))
        }
        None => None,
    };

    let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
    let mut document = if feedback_path.is_file() {
        read_feedback_document(&feedback_path)?
    } else {
        FeedbackDocument {
            reviewer: resolve_reviewer(reviewer_override)?,
            reviewed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            notes: Vec::new(),
            assertion_verdicts: BTreeMap::new(),
        }
    };

    let replaced = document
        .assertion_verdicts
        .insert(assertion_id.to_string(), AssertionVerdict { verdict, rationale })
        .is_some();

    if let Some(parent) = feedback_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&feedback_path, serde_json::to_string_pretty(&document)?)?;

    Ok(FeedbackLabelReport {
        run_id: run_id.to_string(),
        assertion_id: assertion_id.to_string(),
        verdict,
        replaced,
    })
}

pub fn load_run_feedback_entries(report_dir: &Path) -> Result<Vec<RunFeedbackEntry>> {
    let runs = load_report_runs(report_dir)?;
    let mut entries = Vec::new();

    for run in runs {
        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if !feedback_path.is_file() {
            continue;
        }

        let feedback = read_feedback_document(&feedback_path)?;
        let source_path = feedback_path
            .strip_prefix(report_dir)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| feedback_path.display().to_string());
        entries.push(RunFeedbackEntry {
            run_id: run.id,
            eval_case_id: run.eval_case_id,
            scenario_id: run.scenario_id,
            source_path,
            feedback,
        });
    }

    Ok(entries)
}

pub fn summarize_feedback(total_runs: usize, entries: &[RunFeedbackEntry]) -> HumanFeedbackSummary {
    let reviewed_runs = entries.len();
    let mut by_severity = FeedbackCountBySeverity::default();
    let mut by_category = FeedbackCountByCategory::default();

    for entry in entries {
        for note in &entry.feedback.notes {
            match note.severity {
                FeedbackSeverity::Info => by_severity.info += 1,
                FeedbackSeverity::Warning => by_severity.warning += 1,
                FeedbackSeverity::Blocker => by_severity.blocker += 1,
            }
            match note.category {
                FeedbackCategory::Correctness => by_category.correctness += 1,
                FeedbackCategory::Style => by_category.style += 1,
                FeedbackCategory::Completeness => by_category.completeness += 1,
                FeedbackCategory::Other => by_category.other += 1,
            }
        }
    }

    HumanFeedbackSummary {
        total_runs,
        reviewed_runs,
        pending_runs: total_runs.saturating_sub(reviewed_runs),
        by_severity,
        by_category,
    }
}

pub fn collect_improvement_feedback(entries: &[RunFeedbackEntry]) -> Vec<ImprovementFeedbackRecord> {
    entries
        .iter()
        .filter(|entry| !entry.feedback.notes.is_empty())
        .map(|entry| ImprovementFeedbackRecord {
            run_id: entry.run_id.clone(),
            eval_case_id: entry.eval_case_id.clone(),
            scenario_id: entry.scenario_id,
            reviewer: entry.feedback.reviewer.clone(),
            reviewed_at: entry.feedback.reviewed_at.clone(),
            notes: entry.feedback.notes.clone(),
        })
        .collect()
}

pub fn parse_feedback_document(content: &str) -> Result<FeedbackDocument> {
    let document: FeedbackDocument = serde_json::from_str(content)?;
    validate_feedback_document(&document)?;
    Ok(document)
}

fn read_feedback_document(path: &Path) -> Result<FeedbackDocument> {
    let content = std::fs::read_to_string(path)?;
    parse_feedback_document(&content)
}

fn validate_feedback_document(document: &FeedbackDocument) -> Result<()> {
    if document.reviewer.trim().is_empty() {
        return Err(FeedbackError::Message(
            "reviewer must be a non-empty string".to_string(),
        ));
    }

    DateTime::parse_from_rfc3339(&document.reviewed_at).map_err(|_| {
        FeedbackError::Message(format!(
            "reviewed_at '{}' is not a valid RFC3339 timestamp",
            document.reviewed_at
        ))
    })?;

    for (index, note) in document.notes.iter().enumerate() {
        if note.text.trim().is_empty() {
            return Err(FeedbackError::Message(format!(
                "notes[{index}].text must be a non-empty string"
            )));
        }
    }

    for (assertion_id, verdict) in &document.assertion_verdicts {
        if assertion_id.trim().is_empty() {
            return Err(FeedbackError::Message(
                "assertion_verdicts keys must be non-empty assertion ids".to_string(),
            ));
        }
        if verdict.rationale.as_ref().is_some_and(|text| text.trim().is_empty()) {
            return Err(FeedbackError::Message(format!(
                "assertion_verdicts['{assertion_id}'].rationale must be a non-empty string when present"
            )));
        }
    }

    Ok(())
}

/// Whether every `assertion_verdicts` key in `document` names an assertion this run's
/// `grading.json` actually scored, rather than one the reviewer mistyped or one from a
/// different case entirely.
fn validate_assertion_ids_are_graded(
    report_dir: &Path,
    run: &ReportRunRef,
    assertion_ids_by_case: &HashMap<String, Vec<String>>,
    document: &FeedbackDocument,
) -> std::result::Result<(), String> {
    if document.assertion_verdicts.is_empty() {
        return Ok(());
    }

    let graded = graded_assertion_ids(report_dir, run, assertion_ids_by_case).map_err(|e| e.to_string())?;
    for assertion_id in document.assertion_verdicts.keys() {
        if !graded.iter().any(|id| id == assertion_id) {
            return Err(format!(
                "assertion '{assertion_id}' is not a graded assertion for run '{}'",
                run.id
            ));
        }
    }
    Ok(())
}

/// The assertion ids this run's `grading.json` actually produced results for, in the
/// same order `grade_report_bundle` wrote them.
///
/// Positions past what `grading.json` holds are excluded: a run that appended
/// mock-violation or read-only-fixture-violation results carries more grading entries
/// than its case declared graders, and those extra entries have no stable assertion id
/// to be labelled under.
fn graded_assertion_ids(
    report_dir: &Path,
    run: &ReportRunRef,
    assertion_ids_by_case: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>> {
    let grading_path = grading_path_for_run(report_dir, &run.paths.workspace);
    if !grading_path.is_file() {
        return Err(FeedbackError::Message(format!(
            "run '{}' has not been graded yet; run `eval grade` before labelling its assertions",
            run.id
        )));
    }

    let grading: GradingFile = serde_json::from_str(&std::fs::read_to_string(&grading_path)?)?;
    let declared = assertion_ids_by_case
        .get(&run.eval_case_id)
        .cloned()
        .unwrap_or_default();
    Ok(declared.into_iter().take(grading.assertion_results.len()).collect())
}

pub(crate) fn load_report(report_dir: &Path) -> Result<LoadedReport> {
    let report_path = report_dir.join("report.json");
    if !report_path.is_file() {
        return Err(FeedbackError::Message(format!(
            "report.json not found in {}",
            report_dir.display()
        )));
    }

    let content = std::fs::read_to_string(&report_path)?;
    let document: ReportRunsDocument = serde_json::from_str(&content)?;
    if document.runs.is_empty() {
        return Err(FeedbackError::Message("report.json contains no runs".to_string()));
    }

    let assertion_ids_by_case = document
        .dimensions
        .eval_cases
        .into_iter()
        .map(|case| (case.id, case.assertion_ids))
        .collect();

    Ok(LoadedReport {
        runs: document.runs,
        assertion_ids_by_case,
    })
}

fn load_report_runs(report_dir: &Path) -> Result<Vec<ReportRunRef>> {
    Ok(load_report(report_dir)?.runs)
}

fn git_user_email() -> Option<String> {
    std::process::Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use std::path::Path;

    fn sample_report_dir(temp: &tempfile::TempDir) -> PathBuf {
        let fs = MemFS::new();
        let skill_path = Path::new("demo-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "graders": [{ "type": "contains", "text": "assert b" }]
                    }
                ]
            }"#,
        );

        let bundle = build_report_bundle(
            &fs,
            skill_path,
            Path::new("demo-skill"),
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("report-test".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap()
    }

    /// Writes a `grading.json` for each of `sample_report_dir`'s two runs directly,
    /// rather than through `grade_report_bundle`, since a fabricated result is enough to
    /// exercise the assertion-id join and none of these tests need a real runner output.
    fn grade_sample_report(report_dir: &Path) {
        use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};

        for (run_id, assertion_text, passed) in [("run-001", "assert a", true), ("run-002", "assert b", false)] {
            let result = AssertionGradeResult {
                assertion: assertion_text.to_string(),
                passed,
                evidence: format!("evidence for {assertion_text}"),
                grader: GraderInfo {
                    kind: GraderKind::Mechanical,
                    model: None,
                    command: None,
                },
                name: None,
                rationale: None,
                unsupported: None,
                excluded: None,
                ungraded: None,
                votes: None,
                weight: None,
            };
            let grading = build_grading_file(vec![result]).unwrap();
            std::fs::write(
                report_dir.join(format!("runs/{run_id}/grading.json")),
                serde_json::to_string_pretty(&grading).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn parse_feedback_document_accepts_empty_notes() {
        let document = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": []
            }"#,
        )
        .unwrap();

        assert_eq!(document.reviewer, "reviewer@example.com");
        assert!(document.notes.is_empty());
    }

    #[test]
    fn parse_feedback_document_rejects_invalid_severity() {
        let err = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "critical", "category": "correctness", "text": "bad"}
                ]
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("critical") || err.to_string().contains("severity"));
    }

    #[test]
    fn parse_feedback_document_rejects_empty_note_text() {
        let err = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "warning", "category": "style", "text": "   "}
                ]
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("notes[0].text"));
    }

    #[test]
    fn init_creates_feedback_for_each_run() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);

        let report = init_feedback(&report_dir, Some("human@example.com")).unwrap();
        assert_eq!(report.created, 2);
        assert_eq!(report.skipped, 0);

        for run_id in ["run-001", "run-002"] {
            let path = report_dir.join(format!("runs/{run_id}/feedback.json"));
            assert!(path.is_file());
            let document = read_feedback_document(&path).unwrap();
            assert_eq!(document.reviewer, "human@example.com");
            assert!(document.notes.is_empty());
        }
    }

    #[test]
    fn init_skips_existing_feedback_files() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        init_feedback(&report_dir, Some("first@example.com")).unwrap();

        let report = init_feedback(&report_dir, Some("second@example.com")).unwrap();
        assert_eq!(report.created, 0);
        assert_eq!(report.skipped, 2);

        let path = report_dir.join("runs/run-001/feedback.json");
        let document = read_feedback_document(&path).unwrap();
        assert_eq!(document.reviewer, "first@example.com");
    }

    #[test]
    fn list_reports_runs_without_feedback() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);

        let pending = list_runs_needing_review(&report_dir).unwrap();
        assert_eq!(pending, vec!["run-001".to_string(), "run-002".to_string()]);

        init_feedback(&report_dir, Some("human@example.com")).unwrap();
        let pending = list_runs_needing_review(&report_dir).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn validate_reports_schema_errors() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        init_feedback(&report_dir, Some("human@example.com")).unwrap();

        let invalid_path = report_dir.join("runs/run-001/feedback.json");
        std::fs::write(
            &invalid_path,
            r#"{
                "reviewer": "human@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "blocker", "category": "correctness", "text": ""}
                ]
            }"#,
        )
        .unwrap();

        let report = validate_feedback(&report_dir).unwrap();
        assert_eq!(report.validated, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("run run-001"));
    }

    #[test]
    fn summarize_feedback_counts_by_severity_and_category() {
        let entries = vec![RunFeedbackEntry {
            run_id: "run-001".to_string(),
            eval_case_id: "case-a".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            source_path: "runs/run-001/feedback.json".to_string(),
            feedback: FeedbackDocument {
                reviewer: "human@example.com".to_string(),
                reviewed_at: "2026-05-26T12:00:00Z".to_string(),
                notes: vec![
                    FeedbackNote {
                        severity: FeedbackSeverity::Info,
                        category: FeedbackCategory::Style,
                        text: "Minor formatting issue in summary.md".to_string(),
                    },
                    FeedbackNote {
                        severity: FeedbackSeverity::Blocker,
                        category: FeedbackCategory::Correctness,
                        text: "Missing required chart output".to_string(),
                    },
                ],
                assertion_verdicts: BTreeMap::new(),
            },
        }];

        let summary = summarize_feedback(2, &entries);
        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.reviewed_runs, 1);
        assert_eq!(summary.pending_runs, 1);
        assert_eq!(summary.by_severity.info, 1);
        assert_eq!(summary.by_severity.blocker, 1);
        assert_eq!(summary.by_category.style, 1);
        assert_eq!(summary.by_category.correctness, 1);
    }

    #[test]
    fn collect_improvement_feedback_omits_clean_reviews() {
        let entries = vec![
            RunFeedbackEntry {
                run_id: "run-001".to_string(),
                eval_case_id: "case-a".to_string(),
                scenario_id: ScenarioKind::WithSkill,
                source_path: "runs/run-001/feedback.json".to_string(),
                feedback: FeedbackDocument {
                    reviewer: "human@example.com".to_string(),
                    reviewed_at: "2026-05-26T12:00:00Z".to_string(),
                    notes: vec![],
                    assertion_verdicts: BTreeMap::new(),
                },
            },
            RunFeedbackEntry {
                run_id: "run-002".to_string(),
                eval_case_id: "case-b".to_string(),
                scenario_id: ScenarioKind::WithSkill,
                source_path: "runs/run-002/feedback.json".to_string(),
                feedback: FeedbackDocument {
                    reviewer: "human@example.com".to_string(),
                    reviewed_at: "2026-05-26T12:00:00Z".to_string(),
                    notes: vec![FeedbackNote {
                        severity: FeedbackSeverity::Warning,
                        category: FeedbackCategory::Completeness,
                        text: "Add edge-case coverage for empty CSV".to_string(),
                    }],
                    assertion_verdicts: BTreeMap::new(),
                },
            },
        ];

        let records = collect_improvement_feedback(&entries);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run-002");
        assert_eq!(records[0].notes.len(), 1);
    }

    #[test]
    fn parse_feedback_document_accepts_assertion_verdicts() {
        let document = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "fail", "rationale": "evidence quoted the wrong file"}
                }
            }"#,
        )
        .unwrap();

        let verdict = &document.assertion_verdicts["case-a:g0"];
        assert_eq!(verdict.verdict, HumanVerdict::Fail);
        assert_eq!(verdict.rationale.as_deref(), Some("evidence quoted the wrong file"));
    }

    #[test]
    fn parse_feedback_document_rejects_invalid_verdict() {
        let err = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "maybe"}
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("maybe") || err.to_string().contains("verdict"));
    }

    #[test]
    fn parse_feedback_document_rejects_blank_rationale() {
        let err = parse_feedback_document(
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "pass", "rationale": "   "}
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("rationale"));
    }

    #[test]
    fn record_assertion_verdict_creates_feedback_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_sample_report(&report_dir);

        let report = record_assertion_verdict(
            &report_dir,
            "run-001",
            "case-a:g0",
            HumanVerdict::Pass,
            Some("looks right".to_string()),
            Some("reviewer@example.com"),
        )
        .unwrap();
        assert!(!report.replaced);

        let document = read_feedback_document(&report_dir.join("runs/run-001/feedback.json")).unwrap();
        let verdict = &document.assertion_verdicts["case-a:g0"];
        assert_eq!(verdict.verdict, HumanVerdict::Pass);
        assert_eq!(verdict.rationale.as_deref(), Some("looks right"));

        let report = record_assertion_verdict(
            &report_dir,
            "run-001",
            "case-a:g0",
            HumanVerdict::Fail,
            None,
            Some("reviewer@example.com"),
        )
        .unwrap();
        assert!(report.replaced);

        let document = read_feedback_document(&report_dir.join("runs/run-001/feedback.json")).unwrap();
        let verdict = &document.assertion_verdicts["case-a:g0"];
        assert_eq!(verdict.verdict, HumanVerdict::Fail);
        assert!(verdict.rationale.is_none());
        assert_eq!(document.assertion_verdicts.len(), 1);
    }

    #[test]
    fn record_assertion_verdict_rejects_an_unknown_assertion() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_sample_report(&report_dir);

        let err = record_assertion_verdict(
            &report_dir,
            "run-001",
            "case-a:g7",
            HumanVerdict::Pass,
            None,
            Some("reviewer@example.com"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("case-a:g7"));
    }

    #[test]
    fn record_assertion_verdict_rejects_an_ungraded_run() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);

        let err = record_assertion_verdict(
            &report_dir,
            "run-001",
            "case-a:g0",
            HumanVerdict::Pass,
            None,
            Some("reviewer@example.com"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("has not been graded"));
    }

    #[test]
    fn validate_reports_an_assertion_id_the_run_never_graded() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_sample_report(&report_dir);
        init_feedback(&report_dir, Some("human@example.com")).unwrap();

        let feedback_path = report_dir.join("runs/run-001/feedback.json");
        std::fs::write(
            &feedback_path,
            r#"{
                "reviewer": "human@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-b:g0": {"verdict": "pass"}
                }
            }"#,
        )
        .unwrap();

        let report = validate_feedback(&report_dir).unwrap();
        assert_eq!(report.validated, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("case-b:g0"));
    }

    #[test]
    fn validate_accepts_a_verdict_on_a_graded_assertion() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_sample_report(&report_dir);
        init_feedback(&report_dir, Some("human@example.com")).unwrap();

        record_assertion_verdict(
            &report_dir,
            "run-001",
            "case-a:g0",
            HumanVerdict::Pass,
            None,
            Some("human@example.com"),
        )
        .unwrap();

        let report = validate_feedback(&report_dir).unwrap();
        assert_eq!(report.validated, 2);
        assert!(report.errors.is_empty());
    }
}
