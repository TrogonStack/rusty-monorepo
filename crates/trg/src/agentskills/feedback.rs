//! Human review feedback artifacts for skill eval runs.
//!
//! Reviewers record findings in per-run `feedback.json` files. Each note should be
//! **specific and actionable** — cite what failed or could improve, where it
//! appears (artifact path, assertion, or transcript line), and what "good" looks
//! like. Vague notes such as "looks wrong" are not useful for iteration prompts.
//!
//! An empty `notes` array means the run was reviewed with no issues found; that
//! is an explicit signal, not missing data.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::evals::EvalError;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSeverity {
    Info,
    Warning,
    Blocker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    Correctness,
    Style,
    Completeness,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackNote {
    pub severity: FeedbackSeverity,
    pub category: FeedbackCategory,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDocument {
    pub reviewer: String,
    pub reviewed_at: String,
    pub notes: Vec<FeedbackNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFeedbackEntry {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: ScenarioKind,
    pub source_path: String,
    pub feedback: FeedbackDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanFeedbackSummary {
    pub total_runs: usize,
    pub reviewed_runs: usize,
    pub pending_runs: usize,
    pub by_severity: FeedbackCountBySeverity,
    pub by_category: FeedbackCountByCategory,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeedbackCountBySeverity {
    pub info: usize,
    pub warning: usize,
    pub blocker: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeedbackCountByCategory {
    pub correctness: usize,
    pub style: usize,
    pub completeness: usize,
    pub other: usize,
}

/// Per-run feedback preserved for future iteration improvement prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImprovementFeedbackRecord {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: ScenarioKind,
    pub reviewer: String,
    pub reviewed_at: String,
    pub notes: Vec<FeedbackNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReportRunsDocument {
    runs: Vec<ReportRunRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReportRunRef {
    id: String,
    eval_case_id: String,
    scenario_id: ScenarioKind,
    paths: ReportRunPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReportRunPaths {
    workspace: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackInitReport {
    pub created: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackValidateReport {
    pub validated: usize,
    pub errors: Vec<String>,
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

pub fn feedback_path_for_run(report_dir: &Path, workspace_rel: &str) -> PathBuf {
    let workspace = Path::new(workspace_rel);
    let run_rel = workspace
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(workspace_rel));
    report_dir.join(run_rel).join(FEEDBACK_FILE_NAME)
}

pub fn init_feedback(report_dir: &Path, reviewer_override: Option<&str>) -> Result<FeedbackInitReport> {
    let runs = load_report_runs(report_dir)?;
    let reviewer = resolve_reviewer(reviewer_override)?;
    let reviewed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let template = FeedbackDocument {
        reviewer,
        reviewed_at,
        notes: Vec::new(),
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
    let runs = load_report_runs(report_dir)?;
    let mut validated = 0;
    let mut errors = Vec::new();

    for run in runs {
        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if !feedback_path.is_file() {
            continue;
        }

        match read_and_validate_feedback_file(&feedback_path) {
            Ok(()) => validated += 1,
            Err(message) => errors.push(format!("{} (run {}): {}", feedback_path.display(), run.id, message)),
        }
    }

    Ok(FeedbackValidateReport { validated, errors })
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

fn read_and_validate_feedback_file(path: &Path) -> std::result::Result<(), String> {
    read_feedback_document(path).map(|_| ()).map_err(|e| e.to_string())
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

    Ok(())
}

fn load_report_runs(report_dir: &Path) -> Result<Vec<ReportRunRef>> {
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

    Ok(document.runs)
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
                        "assertions": ["assert a"]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "assertions": ["assert b"]
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
                },
            },
        ];

        let records = collect_improvement_feedback(&entries);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run-002");
        assert_eq!(records[0].notes.len(), 1);
    }
}
