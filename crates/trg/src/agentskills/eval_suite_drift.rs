//! Detect when the eval suite (`evals/evals.json`) changed between iterations.

mod constants;
pub use constants::WARNING_KIND;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::evals::{EvalDirName, EvalError, EvalSplit, Result};
use super::report::ReportDocument;
use crate::fs::FileSystem;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct ReportDriftSnapshot {
    pub iteration: u32,
    pub evals_hash: String,
    pub declared_eval_case_ids: BTreeSet<String>,
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
    let value = read_report_value(report_dir)?;
    report_drift_snapshot_from_value(&value)
}

/// Read and parse a report once, so a caller that already has this value (for example one
/// that scanned it as a candidate previous report) never has to open the file again to get
/// a projection of it.
pub fn read_report_value(report_dir: &Path) -> Result<serde_json::Value> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", report_path.display()),
        ))
    })?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

pub fn report_drift_snapshot_from_value(value: &serde_json::Value) -> Result<ReportDriftSnapshot> {
    let iteration = parse_report_iteration(value)?;
    let evals_hash = value
        .pointer("/suite/evals_hash")
        .and_then(|field| field.as_str())
        .unwrap_or_default()
        .to_string();
    let declared_eval_case_ids = recorded_declared_case_ids(value).unwrap_or_else(|| dimension_case_ids(value));

    Ok(ReportDriftSnapshot {
        iteration,
        evals_hash,
        declared_eval_case_ids,
    })
}

/// The suite a narrowed run recorded selecting from, absent when the run covered the
/// whole suite or predates the record.
fn recorded_declared_case_ids(value: &serde_json::Value) -> Option<BTreeSet<String>> {
    let declared: BTreeSet<String> = value
        .pointer("/suite/case_selection/declared")?
        .as_array()?
        .iter()
        .filter_map(declared_case_id)
        .collect();
    (!declared.is_empty()).then_some(declared)
}

/// Reads a `case_selection.declared` entry's id whether it is the current `{id, split}` object
/// or a legacy bare string, so a narrowed report written by either build still contributes its
/// full declared set instead of silently dropping every entry and falling back to only the
/// cases this run covered.
fn declared_case_id(entry: &serde_json::Value) -> Option<String> {
    entry
        .as_str()
        .map(str::to_string)
        .or_else(|| entry.get("id").and_then(|id| id.as_str()).map(str::to_string))
}

fn dimension_case_ids(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .pointer("/dimensions/eval_cases")
        .and_then(|cases| cases.as_array())
        .map(|cases| {
            cases
                .iter()
                .filter_map(|case| case.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This range check reports the offending report field and u32 limit as a validation error; the unit integer-conversion cause adds no information"
    )
)]
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

/// Every case ID the report's suite declared, which is not always what the run covered.
///
/// Drift is a statement about the manifest. A narrowed run's dimensions list only the
/// cases it selected, so diffing those would report a case the run merely left out as one
/// the suite lost, and report it as added again the next time a run selects it.
pub fn declared_eval_case_ids(report: &ReportDocument) -> BTreeSet<String> {
    match &report.suite.case_selection {
        Some(selection) if !selection.declared.is_empty() => {
            selection.declared.iter().map(|case| case.id.clone()).collect()
        }
        _ => report
            .dimensions
            .eval_cases
            .iter()
            .map(|eval_case| eval_case.id.clone())
            .collect(),
    }
}

/// The split every declared case belongs to, covered by this run or not.
///
/// A run narrowed to one split never produces a run for a case the other split declares, so
/// looking up a declared-but-unrun case's split from [`ReportDocument::runs`] silently
/// defaults it (see [`EvalSplit::default`]), which is indistinguishable from a case that
/// really is on the default split. `case_selection.declared` carries the split for every
/// case the suite declared, run or not, so it is consulted first; a full run has no such
/// record because there is nothing narrowed to explain, and every declared case in that
/// case did run, so its split is read off the runs that produced it instead.
///
/// A declared case's split is `None` when it came from a `report.json` an older build wrote
/// before a declared case recorded its split at all; a caller must not treat that the same
/// as a known train split.
pub fn declared_eval_case_splits(report: &ReportDocument) -> HashMap<&str, Option<EvalSplit>> {
    match &report.suite.case_selection {
        Some(selection) if !selection.declared.is_empty() => selection
            .declared
            .iter()
            .map(|case| (case.id.as_str(), case.split))
            .collect(),
        _ => report
            .runs
            .iter()
            .map(|run| (run.eval_case_id.as_str(), Some(run.split)))
            .collect(),
    }
}

pub fn detect_eval_suite_drift(current: &ReportDocument, previous: &ReportDocument) -> Option<EvalSuiteDriftReport> {
    detect_eval_suite_drift_snapshots(
        &ReportDriftSnapshot {
            iteration: current.report.iteration,
            evals_hash: current.suite.evals_hash.clone(),
            declared_eval_case_ids: declared_eval_case_ids(current),
        },
        &ReportDriftSnapshot {
            iteration: previous.report.iteration,
            evals_hash: previous.suite.evals_hash.clone(),
            declared_eval_case_ids: declared_eval_case_ids(previous),
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

    let (added_eval_ids, removed_eval_ids) =
        diff_eval_case_ids(&current.declared_eval_case_ids, &previous.declared_eval_case_ids);

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
    eval_dir: &EvalDirName,
) -> Result<Option<EvalSuiteDriftReport>> {
    detect_eval_suite_drift_vs_skill_with_fs(report, skill_dir, eval_dir, &crate::fs::RealFS)
}

/// Same as [`detect_eval_suite_drift_vs_skill`], but takes the filesystem to resolve the
/// suite through, so a test can inject an IO failure that a real filesystem would not
/// reliably produce on demand (a missing suite, in contrast, is just an absent path).
pub fn detect_eval_suite_drift_vs_skill_with_fs(
    report: &ReportDocument,
    skill_dir: &Path,
    eval_dir: &EvalDirName,
    fs: &impl FileSystem,
) -> Result<Option<EvalSuiteDriftReport>> {
    let compiled = match super::case_directories::resolve_eval_suite(fs, skill_dir, eval_dir) {
        Ok(compiled) => compiled,
        // A missing suite is not drift; anything else the drift check cannot read is not
        // "unchanged", it is unknown, and must not be reported as a clean bill of health.
        Err(EvalError::Io(io_error)) if io_error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let current_hash = compiled.hash;
    let previous_hash = report.suite.evals_hash.clone();

    if current_hash == previous_hash {
        return Ok(None);
    }

    let current_ids: BTreeSet<String> = compiled.suite.evals.iter().map(|eval| eval.id.to_string()).collect();
    let previous_ids = declared_eval_case_ids(report);
    let (added_eval_ids, removed_eval_ids) = diff_eval_case_ids(&current_ids, &previous_ids);

    Ok(Some(EvalSuiteDriftReport {
        current_hash,
        previous_hash,
        added_eval_ids,
        removed_eval_ids,
    }))
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
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

#[cfg(test)]
#[path = "eval_suite_drift_tests.rs"]
mod tests;
