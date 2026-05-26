use crate::fs::FileSystem;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EvalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Validation failed: {}", format_errors(.0))]
    Validation(Vec<String>),
}

fn format_errors(errors: &[String]) -> String {
    errors.join("; ")
}

pub type Result<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct EvalCaseId(String);

impl EvalCaseId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EvalCaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EvalCaseId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EvalCaseIdVisitor;

        impl Visitor<'_> for EvalCaseIdVisitor {
            type Value = EvalCaseId;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a non-empty string or non-negative integer")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(E::custom("eval id must not be empty"));
                }

                Ok(EvalCaseId(trimmed.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(EvalCaseId(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 {
                    return Err(E::custom("eval id must be non-negative"));
                }

                Ok(EvalCaseId(value.to_string()))
            }
        }

        deserializer.deserialize_any(EvalCaseIdVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalSuite {
    pub skill_name: String,
    pub evals: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub id: EvalCaseId,
    pub prompt: String,
    pub expected_output: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub assertions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalCheckOptions {
    pub require_assertions: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCheckReport {
    pub skill_name: String,
    pub eval_count: usize,
    pub file_count: usize,
    pub assertion_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceCheckReport>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceCheckOptions {
    pub require_grading: bool,
    pub fail_on_failed_assertions: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceCheckReport {
    pub grading_files: usize,
    pub timing_files: usize,
    pub assertion_results: usize,
    pub passed_assertions: usize,
    pub failed_assertions: usize,
    pub pass_rate: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GradingFile {
    assertion_results: Vec<AssertionResult>,
    summary: GradingSummary,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionResult {
    text: String,
    passed: bool,
    evidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GradingSummary {
    passed: usize,
    failed: usize,
    total: usize,
    pass_rate: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimingFile {
    total_tokens: u64,
    duration_ms: u64,
}

pub fn check_eval_suite(
    fs: &impl FileSystem,
    skill_path: &Path,
    expected_skill_name: &str,
    options: EvalCheckOptions,
) -> Result<EvalCheckReport> {
    let suite_path = skill_path.join("evals").join("evals.json");
    let content = fs.read_to_string(&suite_path)?;
    let suite: EvalSuite = serde_json::from_str(&content)?;

    let mut errors = Vec::new();
    let mut ids = HashSet::new();
    let mut file_count = 0;
    let mut assertion_count = 0;

    if suite.skill_name.trim().is_empty() {
        errors.push("skill_name must be a non-empty string".to_string());
    } else if suite.skill_name != expected_skill_name {
        errors.push(format!(
            "skill_name '{}' must match skill frontmatter name '{}'",
            suite.skill_name, expected_skill_name
        ));
    }

    if suite.evals.is_empty() {
        errors.push("evals must contain at least one test case".to_string());
    }

    for (index, eval) in suite.evals.iter().enumerate() {
        let label = format!("evals[{}] id '{}'", index, eval.id);

        if !ids.insert(eval.id.as_str().to_string()) {
            errors.push(format!("{} is duplicated", label));
        }

        validate_non_empty(&eval.prompt, &format!("{}.prompt", label), &mut errors);
        validate_non_empty(
            &eval.expected_output,
            &format!("{}.expected_output", label),
            &mut errors,
        );

        if options.require_assertions && eval.assertions.is_empty() {
            errors.push(format!("{} must define at least one assertion", label));
        }

        for (assertion_index, assertion) in eval.assertions.iter().enumerate() {
            validate_non_empty(
                assertion,
                &format!("{}.assertions[{}]", label, assertion_index),
                &mut errors,
            );
        }

        for file in &eval.files {
            file_count += 1;
            validate_eval_file(skill_path, file, fs, &label, &mut errors);
        }

        assertion_count += eval.assertions.len();
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    Ok(EvalCheckReport {
        skill_name: suite.skill_name,
        eval_count: suite.evals.len(),
        file_count,
        assertion_count,
        workspace: None,
    })
}

pub fn check_workspace(workspace_path: &Path, options: WorkspaceCheckOptions) -> Result<WorkspaceCheckReport> {
    if !workspace_path.exists() {
        return Err(EvalError::Validation(vec![format!(
            "workspace '{}' does not exist",
            workspace_path.display()
        )]));
    }

    if !workspace_path.is_dir() {
        return Err(EvalError::Validation(vec![format!(
            "workspace '{}' must be a directory",
            workspace_path.display()
        )]));
    }

    let mut grading_files = Vec::new();
    let mut timing_files = Vec::new();
    collect_named_files(workspace_path, "grading.json", &mut grading_files)?;
    collect_named_files(workspace_path, "timing.json", &mut timing_files)?;

    let mut errors = Vec::new();
    let mut assertion_results = 0;
    let mut passed_assertions = 0;
    let mut failed_assertions = 0;

    if options.require_grading && grading_files.is_empty() {
        errors.push(format!(
            "workspace '{}' must contain at least one grading.json",
            workspace_path.display()
        ));
    }

    for grading_path in &grading_files {
        let grading = read_grading_file(grading_path)?;
        validate_grading_file(grading_path, &grading, options, &mut errors);

        for result in grading.assertion_results {
            assertion_results += 1;
            if result.passed {
                passed_assertions += 1;
            } else {
                failed_assertions += 1;
            }
        }
    }

    for timing_path in &timing_files {
        let timing = read_timing_file(timing_path)?;
        validate_timing_file(timing_path, &timing, &mut errors);
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    let pass_rate = if assertion_results == 0 {
        0.0
    } else {
        passed_assertions as f64 / assertion_results as f64
    };

    Ok(WorkspaceCheckReport {
        grading_files: grading_files.len(),
        timing_files: timing_files.len(),
        assertion_results,
        passed_assertions,
        failed_assertions,
        pass_rate,
    })
}

fn validate_non_empty(value: &str, field: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        errors.push(format!("{} must be a non-empty string", field));
    }
}

fn validate_eval_file(skill_path: &Path, file: &str, fs: &impl FileSystem, label: &str, errors: &mut Vec<String>) {
    if file.trim().is_empty() {
        errors.push(format!("{}.files contains an empty path", label));
        return;
    }

    let path = Path::new(file);
    if !is_safe_relative_path(path) {
        errors.push(format!(
            "{}.files path '{}' must stay inside the skill directory",
            label, file
        ));
        return;
    }

    let full_path = skill_path.join(path);
    if !fs.exists(&full_path) {
        errors.push(format!("{}.files path '{}' does not exist", label, full_path.display()));
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }

    path.components()
        .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
}

fn collect_named_files(root: &Path, file_name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            collect_named_files(&path, file_name, matches)?;
        } else if file_type.is_file() && path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            matches.push(path);
        }
    }

    matches.sort();
    Ok(())
}

fn read_grading_file(path: &Path) -> Result<GradingFile> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn read_timing_file(path: &Path) -> Result<TimingFile> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn validate_grading_file(path: &Path, grading: &GradingFile, options: WorkspaceCheckOptions, errors: &mut Vec<String>) {
    if grading.assertion_results.is_empty() {
        errors.push(format!(
            "{} assertion_results must contain at least one result",
            path.display()
        ));
    }

    let passed = grading.assertion_results.iter().filter(|result| result.passed).count();
    let failed = grading.assertion_results.len() - passed;

    for (index, result) in grading.assertion_results.iter().enumerate() {
        validate_non_empty(
            &result.text,
            &format!("{} assertion_results[{}].text", path.display(), index),
            errors,
        );
        validate_non_empty(
            &result.evidence,
            &format!("{} assertion_results[{}].evidence", path.display(), index),
            errors,
        );
    }

    if grading.summary.passed != passed {
        errors.push(format!(
            "{} summary.passed {} does not match {} passed assertion results",
            path.display(),
            grading.summary.passed,
            passed
        ));
    }

    if grading.summary.failed != failed {
        errors.push(format!(
            "{} summary.failed {} does not match {} failed assertion results",
            path.display(),
            grading.summary.failed,
            failed
        ));
    }

    if grading.summary.total != grading.assertion_results.len() {
        errors.push(format!(
            "{} summary.total {} does not match {} assertion results",
            path.display(),
            grading.summary.total,
            grading.assertion_results.len()
        ));
    }

    let expected_rate = if grading.summary.total == 0 {
        0.0
    } else {
        passed as f64 / grading.summary.total as f64
    };
    if (grading.summary.pass_rate - expected_rate).abs() > 0.0001 {
        errors.push(format!(
            "{} summary.pass_rate {} does not match computed pass rate {}",
            path.display(),
            grading.summary.pass_rate,
            expected_rate
        ));
    }

    if options.fail_on_failed_assertions && failed > 0 {
        errors.push(format!("{} has {} failed assertion result(s)", path.display(), failed));
    }
}

fn validate_timing_file(path: &Path, timing: &TimingFile, errors: &mut Vec<String>) {
    if timing.total_tokens == 0 {
        errors.push(format!("{} total_tokens must be greater than zero", path.display()));
    }

    if timing.duration_ms == 0 {
        errors.push(format!("{} duration_ms must be greater than zero", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::MemFS;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn check_eval_suite_accepts_valid_manifest() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/csv-analyzer/evals/evals.json"),
            r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": 1,
      "prompt": "Analyze evals/files/sales.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/sales.csv"],
      "assertions": ["The output includes a summary"]
    }
  ]
}"#,
        );
        fs.insert(
            Path::new("/csv-analyzer/evals/files/sales.csv"),
            "month,revenue\nMay,10",
        );

        let report = check_eval_suite(
            &fs,
            Path::new("/csv-analyzer"),
            "csv-analyzer",
            EvalCheckOptions {
                require_assertions: true,
            },
        )
        .unwrap();

        assert_eq!(report.skill_name, "csv-analyzer");
        assert_eq!(report.eval_count, 1);
        assert_eq!(report.file_count, 1);
        assert_eq!(report.assertion_count, 1);
    }

    #[test]
    fn check_eval_suite_rejects_missing_fixture_file() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/csv-analyzer/evals/evals.json"),
            r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "missing-file",
      "prompt": "Analyze evals/files/missing.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/missing.csv"]
    }
  ]
}"#,
        );

        let err = check_eval_suite(
            &fs,
            Path::new("/csv-analyzer"),
            "csv-analyzer",
            EvalCheckOptions::default(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn check_eval_suite_rejects_paths_outside_skill_directory() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/csv-analyzer/evals/evals.json"),
            r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "outside",
      "prompt": "Analyze a fixture",
      "expected_output": "A short summary.",
      "files": ["../outside.csv"]
    }
  ]
}"#,
        );

        let err = check_eval_suite(
            &fs,
            Path::new("/csv-analyzer"),
            "csv-analyzer",
            EvalCheckOptions::default(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("must stay inside the skill directory"));
    }

    #[test]
    fn check_eval_suite_requires_assertions_when_requested() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/csv-analyzer/evals/evals.json"),
            r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "no-assertions",
      "prompt": "Analyze input",
      "expected_output": "A short summary."
    }
  ]
}"#,
        );

        let err = check_eval_suite(
            &fs,
            Path::new("/csv-analyzer"),
            "csv-analyzer",
            EvalCheckOptions {
                require_assertions: true,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("must define at least one assertion"));
    }

    #[test]
    fn check_workspace_aggregates_grading_and_timing_files() {
        let tmp = tempdir().unwrap();
        let run = tmp.path().join("iteration-1").join("eval-one").join("with_skill");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("grading.json"),
            r#"{
  "assertion_results": [
    { "text": "Includes a summary", "passed": true, "evidence": "summary.md exists" },
    { "text": "Includes a chart", "passed": false, "evidence": "no chart file found" }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
        )
        .unwrap();
        fs::write(
            run.join("timing.json"),
            r#"{ "total_tokens": 1000, "duration_ms": 2500 }"#,
        )
        .unwrap();

        let report = check_workspace(tmp.path(), WorkspaceCheckOptions::default()).unwrap();

        assert_eq!(report.grading_files, 1);
        assert_eq!(report.timing_files, 1);
        assert_eq!(report.assertion_results, 2);
        assert_eq!(report.passed_assertions, 1);
        assert_eq!(report.failed_assertions, 1);
        assert_eq!(report.pass_rate, 0.5);
    }

    #[test]
    fn check_workspace_can_fail_on_failed_assertions() {
        let tmp = tempdir().unwrap();
        let run = tmp.path().join("iteration-1").join("eval-one").join("with_skill");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("grading.json"),
            r#"{
  "assertion_results": [
    { "text": "Includes a summary", "passed": false, "evidence": "summary.md missing" }
  ],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
        )
        .unwrap();

        let err = check_workspace(
            tmp.path(),
            WorkspaceCheckOptions {
                require_grading: true,
                fail_on_failed_assertions: true,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("failed assertion"));
    }
}
