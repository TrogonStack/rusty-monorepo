mod constants;
pub use constants::{
    BENCHMARK_SCHEMA, COMPARISON_SCHEMA, ENV_SCHEMA, EVALS_SCHEMA, EVENTS_SCHEMA, FEEDBACK_SCHEMA, GRADING_SCHEMA,
    IMPROVEMENT_BUNDLE_SCHEMA, ITERATION_SUMMARY_SCHEMA, REPORT_SCHEMA, TIMING_SCHEMA,
};
use std::path::Path;

use super::evals::{collect_named_files, EvalError, Result};
use super::feedback::FEEDBACK_FILE_NAME;
use super::transcript::NORMALIZED_TRANSCRIPT_FILE;
use super::validation::ValidationError;

/// Whether this build of `trg` can hold an artifact against its schema.
///
/// Schema validation is a Cargo feature. A build without it turns every check into a
/// pass, which is the one answer a verification command must never give quietly: the
/// operator reads a clean exit as proof the bundle conforms, when nothing looked at it.
/// Carried as a value so a command can refuse rather than silently answer for a check it
/// did not make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaValidation {
    /// The validator is compiled in, so a check is a check.
    Compiled,
    /// The validator was compiled out, so every artifact would pass unexamined.
    Absent,
}

impl SchemaValidation {
    pub const fn of_this_build() -> Self {
        if cfg!(any(feature = "schema-validation", test)) {
            Self::Compiled
        } else {
            Self::Absent
        }
    }

    pub fn is_compiled(self) -> bool {
        matches!(self, Self::Compiled)
    }
}

pub fn validate_artifact(schema: &str, json: &serde_json::Value) -> Result<()> {
    #[cfg(any(feature = "schema-validation", test))]
    {
        let schema_value: serde_json::Value = serde_json::from_str(schema)?;
        let validator = jsonschema::validator_for(&schema_value)
            .map_err(|error| EvalError::Validation(ValidationError::for_field("schema", error.to_string()).into()))?;

        let errors: Vec<String> = validator.iter_errors(json).map(|error| error.to_string()).collect();

        if errors.is_empty() {
            return Ok(());
        }

        Err(EvalError::Validation(
            ValidationError::for_field("artifact", errors.join("; ")).into(),
        ))
    }

    #[cfg(not(any(feature = "schema-validation", test)))]
    {
        let _ = (schema, json);
        Ok(())
    }
}

pub fn validate_report_bundle_schemas(report_dir: &Path) -> Result<()> {
    let report_path = report_dir.join("report.json");
    if report_path.is_file() {
        let value = read_json_file(&report_path)?;
        validate_artifact(REPORT_SCHEMA, &value)?;
    }

    validate_named_artifacts(report_dir, "grading.json", GRADING_SCHEMA)?;
    validate_named_artifacts(report_dir, "timing.json", TIMING_SCHEMA)?;
    validate_named_artifacts(report_dir, "env.json", ENV_SCHEMA)?;
    validate_named_artifacts(report_dir, NORMALIZED_TRANSCRIPT_FILE, EVENTS_SCHEMA)?;
    validate_named_artifacts(report_dir, FEEDBACK_FILE_NAME, FEEDBACK_SCHEMA)?;
    validate_named_artifacts(report_dir, "comparison.json", COMPARISON_SCHEMA)?;
    validate_benchmark_artifacts(report_dir)?;
    validate_iteration_summary_artifacts(report_dir)?;
    Ok(())
}

fn validate_named_artifacts(root: &Path, file_name: &str, schema: &str) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(root, file_name, &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        validate_artifact(schema, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn validate_benchmark_artifacts(report_dir: &Path) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(report_dir, "benchmark.json", &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        if value.get("report_id").is_none() {
            continue;
        }
        validate_artifact(BENCHMARK_SCHEMA, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn validate_iteration_summary_artifacts(report_dir: &Path) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(report_dir, "iteration-summary.json", &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        if value.get("report_id").is_none() {
            continue;
        }
        validate_artifact(ITERATION_SUMMARY_SCHEMA, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", path.display()),
        ))
    })?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

fn schema_error_for_path(path: &Path, error: EvalError) -> EvalError {
    match error {
        EvalError::Validation(errors) => {
            EvalError::Validation(ValidationError::for_field(path.display().to_string(), errors.to_string()).into())
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "schemas_tests.rs"]
mod tests;
