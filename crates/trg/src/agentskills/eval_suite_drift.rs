//! Detect when the eval suite (`evals/evals.json`) changed between iterations.

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::evals::{parse_eval_suite, EvalError, Result};
use super::report::ReportDocument;

pub const WARNING_KIND: &str = "eval_suite_drift";

#[derive(Debug, Clone)]
pub struct ReportDriftSnapshot {
    pub iteration: u32,
    pub evals_hash: String,
    pub eval_case_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSuiteDriftReport {
    pub current_hash: String,
    pub previous_hash: String,
    pub added_eval_ids: Vec<String>,
    pub removed_eval_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, JsonSchema)]
pub struct EvalSuiteDriftWarning {
    pub kind: String,
    pub current_hash: String,
    pub previous_hash: String,
    pub added_eval_ids: Vec<String>,
    pub removed_eval_ids: Vec<String>,
}

impl From<&EvalSuiteDriftReport> for EvalSuiteDriftWarning {
    fn from(drift: &EvalSuiteDriftReport) -> Self {
        Self {
            kind: WARNING_KIND.to_string(),
            current_hash: drift.current_hash.clone(),
            previous_hash: drift.previous_hash.clone(),
            added_eval_ids: drift.added_eval_ids.clone(),
            removed_eval_ids: drift.removed_eval_ids.clone(),
        }
    }
}

pub fn load_report_document(report_dir: &Path) -> Result<ReportDocument> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", report_path.display()),
        ))
    })?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

pub fn load_report_drift_snapshot(report_dir: &Path) -> Result<ReportDriftSnapshot> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", report_path.display()),
        ))
    })?;
    let value: serde_json::Value = serde_json::from_str(&content)?;

    let iteration = parse_report_iteration(&value)?;
    let evals_hash = value
        .pointer("/suite/evals_hash")
        .and_then(|field| field.as_str())
        .unwrap_or_default()
        .to_string();
    let eval_case_ids = value
        .pointer("/dimensions/eval_cases")
        .and_then(|cases| cases.as_array())
        .map(|cases| {
            cases
                .iter()
                .filter_map(|case| case.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Ok(ReportDriftSnapshot {
        iteration,
        evals_hash,
        eval_case_ids,
    })
}

pub fn parse_report_iteration(value: &serde_json::Value) -> Result<u32> {
    let raw = value
        .pointer("/report/iteration")
        .and_then(|field| {
            field.as_u64().or_else(|| {
                field.as_object().and_then(|object| {
                    ["index", "iteration"]
                        .iter()
                        .find_map(|key| object.get(*key).and_then(|value| value.as_u64()))
                })
            })
        })
        .unwrap_or(1);

    raw.try_into().map_err(|_| {
        EvalError::Validation(
            super::validation::ValidationError::for_field("report.iteration", format!("value {raw} exceeds u32::MAX"))
                .into(),
        )
    })
}

pub fn eval_case_ids(report: &ReportDocument) -> BTreeSet<String> {
    report
        .dimensions
        .eval_cases
        .iter()
        .map(|eval_case| eval_case.id.clone())
        .collect()
}

pub fn detect_eval_suite_drift(current: &ReportDocument, previous: &ReportDocument) -> Option<EvalSuiteDriftReport> {
    detect_eval_suite_drift_snapshots(
        &ReportDriftSnapshot {
            iteration: current.report.iteration,
            evals_hash: current.suite.evals_hash.clone(),
            eval_case_ids: eval_case_ids(current),
        },
        &ReportDriftSnapshot {
            iteration: previous.report.iteration,
            evals_hash: previous.suite.evals_hash.clone(),
            eval_case_ids: eval_case_ids(previous),
        },
    )
}

pub fn detect_eval_suite_drift_snapshots(
    current: &ReportDriftSnapshot,
    previous: &ReportDriftSnapshot,
) -> Option<EvalSuiteDriftReport> {
    if current.evals_hash == previous.evals_hash {
        return None;
    }

    let (added_eval_ids, removed_eval_ids) = diff_eval_case_ids(&current.eval_case_ids, &previous.eval_case_ids);

    Some(EvalSuiteDriftReport {
        current_hash: current.evals_hash.clone(),
        previous_hash: previous.evals_hash.clone(),
        added_eval_ids,
        removed_eval_ids,
    })
}

pub fn detect_eval_suite_drift_vs_skill(
    report: &ReportDocument,
    skill_dir: &Path,
) -> Result<Option<EvalSuiteDriftReport>> {
    let evals_path = skill_dir.join("evals").join("evals.json");
    if !evals_path.is_file() {
        return Ok(None);
    }

    let current_content = std::fs::read_to_string(&evals_path)?;
    let current_hash = sha256_digest(&current_content);
    let previous_hash = report.suite.evals_hash.clone();

    if current_hash == previous_hash {
        return Ok(None);
    }

    let suite = parse_eval_suite(&current_content)?;
    let current_ids: BTreeSet<String> = suite.evals.iter().map(|eval| eval.id.to_string()).collect();
    let previous_ids = eval_case_ids(report);
    let (added_eval_ids, removed_eval_ids) = diff_eval_case_ids(&current_ids, &previous_ids);

    Ok(Some(EvalSuiteDriftReport {
        current_hash,
        previous_hash,
        added_eval_ids,
        removed_eval_ids,
    }))
}

pub fn emit_eval_suite_drift_warning(drift: &EvalSuiteDriftReport) {
    eprintln!(
        "WARN: eval suite changed between iterations (previous {}, current {})",
        drift.previous_hash, drift.current_hash
    );
    if !drift.added_eval_ids.is_empty() {
        eprintln!("  added eval IDs: {}", drift.added_eval_ids.join(", "));
    }
    if !drift.removed_eval_ids.is_empty() {
        eprintln!("  removed eval IDs: {}", drift.removed_eval_ids.join(", "));
    }
}

pub fn maybe_emit_eval_suite_drift_warning(drift: Option<&EvalSuiteDriftReport>, allow: bool) {
    if let Some(drift) = drift {
        if !allow {
            emit_eval_suite_drift_warning(drift);
        }
    }
}

fn diff_eval_case_ids(current_ids: &BTreeSet<String>, previous_ids: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let added_eval_ids: Vec<String> = current_ids.difference(previous_ids).cloned().collect();
    let removed_eval_ids: Vec<String> = previous_ids.difference(current_ids).cloned().collect();
    (added_eval_ids, removed_eval_ids)
}

fn sha256_digest(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{}", super::hex_encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use std::path::Path;

    fn sample_skill(fs: &MemFS, evals_json: &str) -> std::path::PathBuf {
        let skill_path = Path::new("demo-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(skill_path.join("evals/evals.json"), evals_json);
        skill_path.to_path_buf()
    }

    fn report_from_skill(fs: &MemFS, skill_path: &Path, iteration: u32) -> ReportDocument {
        let bundle = build_report_bundle(
            fs,
            skill_path,
            skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                iteration: Some(iteration),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();
        bundle.document
    }

    #[test]
    fn identical_evals_hash_returns_none() {
        let fs = MemFS::new();
        let evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "assertions": ["a"] }
            ]
        }"#;
        let skill_path = sample_skill(&fs, evals);
        let current = report_from_skill(&fs, &skill_path, 2);
        let previous = report_from_skill(&fs, &skill_path, 1);
        assert!(detect_eval_suite_drift(&current, &previous).is_none());
    }

    #[test]
    fn differing_evals_hash_reports_added_and_removed_ids() {
        let fs = MemFS::new();
        let previous_evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "assertions": ["a"] },
                { "id": "case-b", "prompt": "p", "expected_output": "o", "assertions": ["b"] }
            ]
        }"#;
        let current_evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "assertions": ["a"] },
                { "id": "case-c", "prompt": "p", "expected_output": "o", "assertions": ["c"] }
            ]
        }"#;

        let previous_path = Path::new("demo-skill-prev");
        let current_path = Path::new("demo-skill-next");
        fs.insert(
            previous_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(previous_path.join("evals/evals.json"), previous_evals);
        fs.insert(
            current_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(current_path.join("evals/evals.json"), current_evals);
        let previous = report_from_skill(&fs, previous_path, 1);
        let current = report_from_skill(&fs, current_path, 2);

        let drift = detect_eval_suite_drift(&current, &previous).expect("drift");
        assert_ne!(drift.current_hash, drift.previous_hash);
        assert_eq!(drift.added_eval_ids, vec!["case-c".to_string()]);
        assert_eq!(drift.removed_eval_ids, vec!["case-b".to_string()]);
    }

    #[test]
    fn parse_report_iteration_accepts_object_iteration_key() {
        let value = serde_json::json!({
            "report": { "iteration": { "id": "iter-4", "iteration": 4 } }
        });
        assert_eq!(parse_report_iteration(&value).unwrap(), 4);
    }

    #[test]
    fn load_report_drift_snapshot_rejects_iteration_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let report = serde_json::json!({
            "report": { "iteration": (u32::MAX as u64) + 1 },
            "suite": { "evals_hash": "sha256:abc" },
            "dimensions": { "eval_cases": [] }
        });
        std::fs::write(temp.path().join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();

        let error = load_report_drift_snapshot(temp.path()).unwrap_err();
        assert!(error.to_string().contains("report.iteration"));
    }

    #[test]
    fn write_report_fixtures_round_trip_for_drift_tests() {
        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = sample_skill(
            &fs,
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "p", "expected_output": "o", "assertions": ["a"] }
                ]
            }"#,
        );
        let bundle = build_report_bundle(
            &fs,
            &skill_path,
            &skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions::default(),
        )
        .unwrap();
        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
        let loaded = load_report_document(&report_dir).unwrap();
        assert_eq!(loaded.suite.evals_hash, bundle.document.suite.evals_hash);
    }
}
