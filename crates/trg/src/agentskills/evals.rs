use super::graders::CaseGrader;
use super::grading::{self, GradingFile};
use super::outputs::guess_mime_type;
use super::runner::TimingFile;
use super::validation::{ValidationError, ValidationErrors};
use super::workspace_scaffold::WorkspaceScaffold;
use crate::fs::FileSystem;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Directory, relative to the skill root, that holds the eval suite and its fixtures.
///
/// This directory is the answer key. It must never be staged into a run's workspace:
/// see `runner::is_withheld_from_staging`.
pub const EVAL_SUITE_DIR_NAME: &str = "evals";
pub const EVAL_SUITE_MANIFEST_NAME: &str = "evals.json";

pub const DEFAULT_MAX_FIXTURE_BYTES: u64 = 5 * 1024 * 1024;
const FIXTURE_BINARY_SAMPLE_BYTES: usize = 8 * 1024;

#[derive(Error, Debug)]
pub enum EvalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Validation failed: {0}")]
    Validation(ValidationErrors),
}

pub type Result<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[schemars(schema_with = "eval_case_id_schema")]
pub struct EvalCaseId(String);

fn eval_case_id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "Non-empty string or non-negative integer",
        "oneOf": [
            { "type": "string", "minLength": 1 },
            { "type": "integer", "minimum": 0 }
        ]
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[schemars(schema_with = "non_empty_string_schema")]
pub struct NonEmptyString(String);

fn non_empty_string_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1
    })
}

impl NonEmptyString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NonEmptyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(de::Error::custom("must be a non-empty string"));
        }
        Ok(NonEmptyString(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct RelativeSkillPath(String);

impl RelativeSkillPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for RelativeSkillPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RelativeSkillPath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(de::Error::custom("path must not be empty"));
        }
        let path = Path::new(&value);
        if path.is_absolute() {
            return Err(de::Error::custom(format!(
                "path '{}' must be relative to the skill directory",
                value
            )));
        }
        if !path
            .components()
            .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
        {
            return Err(de::Error::custom(format!(
                "path '{}' must stay inside the skill directory",
                value
            )));
        }
        Ok(RelativeSkillPath(value))
    }
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalPriority {
    Low,
    Normal,
    High,
    Critical,
}

/// Whether a case's prompt names the skill that is staged for it.
///
/// A prompt that names the skill asks how well a run uses a skill it has already
/// been handed. It cannot also ask whether the skill's own description wins the
/// run's routing decision, because the prompt made that decision for the run.
/// Those are two different questions, so a case has to say which one it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillDisclosure {
    /// Name the skill, the directory it is staged in, and its description.
    #[default]
    Announced,
    /// Stage the skill where a listing of the workspace shows it and say nothing
    /// about it, so both arms of a comparison get the same prompt and a run has
    /// to reach for the skill on its own.
    Unannounced,
}

impl SkillDisclosure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Announced => "announced",
            Self::Unannounced => "unannounced",
        }
    }

    pub fn announces_the_skill(self) -> bool {
        matches!(self, Self::Announced)
    }
}

fn is_announced(disclosure: &SkillDisclosure) -> bool {
    disclosure.announces_the_skill()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalSuite {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub skill_name: NonEmptyString,
    #[serde(deserialize_with = "deserialize_evals")]
    pub evals: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub id: EvalCaseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<NonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<NonEmptyString>,
    pub prompt: NonEmptyString,
    pub expected_output: NonEmptyString,
    #[serde(default)]
    pub files: Vec<RelativeSkillPath>,
    #[serde(default)]
    pub assertions: Vec<NonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<EvalPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grader_hints: Option<HashMap<String, serde_json::Value>>,
    #[serde(
        default,
        deserialize_with = "deserialize_case_graders",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub graders: Vec<CaseGrader>,
    #[serde(default, skip_serializing_if = "is_announced")]
    pub skill_disclosure: SkillDisclosure,
    /// The state this case is asking about, for a case that is not asking about an empty
    /// directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaffold: Option<WorkspaceScaffold>,
}

impl EvalCase {
    /// Whether the case states any checkable property at all, in either the
    /// legacy natural-language form or the typed form.
    pub fn has_checks(&self) -> bool {
        !self.assertions.is_empty() || !self.graders.is_empty()
    }

    /// Whether any of the case's checks is about the run reaching for the skill,
    /// which is the one question an announcing prompt answers for it.
    pub fn checks_skill_engagement(&self) -> bool {
        self.graders
            .iter()
            .any(|declared| declared.grader.presupposes_the_skill())
    }
}

pub fn parse_eval_suite(content: &str) -> Result<EvalSuite> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    serde_json::from_value(value).map_err(EvalError::from)
}

pub fn effective_timeout_secs(case: &EvalCase, global_timeout_secs: Option<u64>) -> Option<u64> {
    case.timeout_secs.map(u64::from).or(global_timeout_secs)
}

fn deserialize_evals<'de, D>(deserializer: D) -> std::result::Result<Vec<EvalCase>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut evals = Vec::with_capacity(raw.len());
    for (index, case) in raw.into_iter().enumerate() {
        evals.push(
            serde_json::from_value::<EvalCase>(case)
                .map_err(|error| de::Error::custom(format!("evals[{index}]: {error}")))?,
        );
    }
    if evals.is_empty() {
        return Err(de::Error::custom("evals must contain at least one test case"));
    }
    let mut seen = HashSet::new();
    for eval in &evals {
        if !seen.insert(eval.id.as_str()) {
            return Err(de::Error::custom(format!("evals contains duplicate id '{}'", eval.id)));
        }
    }
    Ok(evals)
}

fn deserialize_case_graders<'de, D>(deserializer: D) -> std::result::Result<Vec<CaseGrader>, D::Error>
where
    D: Deserializer<'de>,
{
    let graders = Vec::<CaseGrader>::deserialize(deserializer)?;
    let mut seen = HashSet::new();
    for declared in &graders {
        if let Some(name) = &declared.name {
            if !seen.insert(name.as_str()) {
                return Err(de::Error::custom(format!("graders contains duplicate name '{name}'")));
            }
        }
    }
    Ok(graders)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalCheckOptions {
    pub require_assertions: bool,
    pub max_fixture_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalLintOptions {
    pub allow_empty_assertions: bool,
    pub max_fixture_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalLintWarning {
    pub eval_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WorkspaceCheckReport {
    pub grading_files: usize,
    pub timing_files: usize,
    /// Scored results only: results the runner could not answer are counted in
    /// `unsupported_assertions`, results that presuppose the skill in
    /// `excluded_assertions`, and neither reaches `pass_rate`.
    pub assertion_results: usize,
    pub passed_assertions: usize,
    pub failed_assertions: usize,
    #[serde(default)]
    pub unsupported_assertions: usize,
    #[serde(default)]
    pub excluded_assertions: usize,
    /// `null` when the workspace produced no scored assertion at all, so an
    /// unobservable runner is not reported as a total failure.
    pub pass_rate: Option<f64>,
}

pub fn load_eval_suite(fs: &impl FileSystem, skill_path: &Path) -> Result<EvalSuite> {
    let suite_path = skill_path.join(EVAL_SUITE_DIR_NAME).join(EVAL_SUITE_MANIFEST_NAME);
    let content = fs.read_to_string(&suite_path)?;
    parse_eval_suite(&content)
}

pub fn eval_manifest_scaffold_json(skill_name: &str) -> String {
    let skill_name_json = serde_json::to_string(skill_name).expect("string serialization to JSON is infallible");
    format!(
        r#"{{
  "skill_name": {skill_name_json},
  "evals": [
    {{
      "id": "produces-a-summary",
      "prompt": "Summarize the sample task using the skill guidance and write the result to outputs/summary.md with a top-level Markdown heading.",
      "expected_output": "outputs/summary.md exists and opens with a top-level Markdown heading.",
      "files": [],
      "graders": [
        {{ "type": "file_exists", "path": "summary.md" }},
        {{
          "type": "regex",
          "pattern": "^#[ \\t]+\\S",
          "target": {{ "file": "summary.md" }}
        }},
        {{
          "type": "contains",
          "text": "TODO",
          "target": {{ "file": "summary.md" }},
          "negate": true
        }}
      ]
    }},
    {{
      "id": "triggers-the-skill",
      "prompt": "Replace this with the request a user would make in their own words for the work this skill does, mentioning neither the skill nor where it lives.",
      "expected_output": "The run finds the skill and follows it rather than improvising.",
      "files": [],
      "tags": ["triggering"],
      "skill_disclosure": "unannounced",
      "graders": [
        {{ "type": "skill_used" }}
      ]
    }}
  ]
}}"#
    )
}

pub fn scaffold_eval_suite(skill_name: &str) -> EvalSuite {
    parse_eval_suite(&eval_manifest_scaffold_json(skill_name)).expect("scaffold manifest must parse")
}

pub fn write_eval_suite(fs: &impl FileSystem, skill_path: &Path, suite: &EvalSuite) -> Result<()> {
    let suite_path = skill_path.join(EVAL_SUITE_DIR_NAME).join(EVAL_SUITE_MANIFEST_NAME);
    let json = serde_json::to_string_pretty(suite)?;
    fs.write(&suite_path, &json)?;
    Ok(())
}

pub fn write_eval_manifest_scaffold(fs: &impl FileSystem, skill_path: &Path, skill_name: &str) -> Result<()> {
    let suite_path = skill_path.join(EVAL_SUITE_DIR_NAME).join(EVAL_SUITE_MANIFEST_NAME);
    fs.write(&suite_path, &eval_manifest_scaffold_json(skill_name))?;
    Ok(())
}

pub fn lint_eval_suite(suite: &EvalSuite, options: EvalLintOptions) -> Vec<EvalLintWarning> {
    let mut warnings = Vec::new();

    for eval in &suite.evals {
        let eval_id = eval.id.as_str().to_string();

        if eval.prompt.as_str().len() < 20 {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "prompt is too vague (shorter than 20 characters)".to_string(),
            });
        }

        if eval.expected_output.as_str().len() < 10 {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "expected_output is too generic (shorter than 10 characters)".to_string(),
            });
        }

        if !eval.files.is_empty() && !eval.prompt.as_str().contains('/') {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "fixture files are present but the prompt does not reference a file path".to_string(),
            });
        }

        let mut seen_files = HashSet::new();
        for file in &eval.files {
            if !seen_files.insert(file.as_str()) {
                warnings.push(EvalLintWarning {
                    eval_id: eval_id.clone(),
                    message: format!("duplicate fixture path '{}'", file),
                });
            }
        }

        if eval.checks_skill_engagement() && eval.skill_disclosure.announces_the_skill() {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: format!(
                    "the case checks whether the run reaches for the skill, but the prompt names the skill and its directory; set \"skill_disclosure\": \"{}\" to leave the routing decision to the run",
                    SkillDisclosure::Unannounced.as_str()
                ),
            });
        }

        if !options.allow_empty_assertions && !eval.has_checks() {
            warnings.push(EvalLintWarning {
                eval_id,
                message: "neither assertions nor graders are declared".to_string(),
            });
        }
    }

    warnings
}

pub fn lint_eval_suite_fixtures(
    fs: &impl FileSystem,
    skill_path: &Path,
    suite: &EvalSuite,
    options: EvalLintOptions,
) -> Vec<EvalLintWarning> {
    let mut warnings = lint_eval_suite(suite, options);

    for eval in &suite.evals {
        let eval_id = eval.id.as_str().to_string();
        warnings.extend(lint_fixture_files(
            fs,
            skill_path,
            &eval_id,
            &eval.files,
            options.max_fixture_bytes,
        ));
    }

    warnings
}

fn effective_max_fixture_bytes(max_fixture_bytes: Option<u64>) -> u64 {
    max_fixture_bytes.unwrap_or(DEFAULT_MAX_FIXTURE_BYTES)
}

fn lint_fixture_files(
    fs: &impl FileSystem,
    skill_path: &Path,
    eval_id: &str,
    files: &[RelativeSkillPath],
    max_fixture_bytes: Option<u64>,
) -> Vec<EvalLintWarning> {
    let limit = effective_max_fixture_bytes(max_fixture_bytes);
    let mut warnings = Vec::new();

    for file in files {
        let full_path = skill_path.join(file.as_path());
        if !fs.is_file(&full_path) {
            continue;
        }

        let Ok(bytes) = fs.read_bytes(&full_path) else {
            continue;
        };

        if bytes.len() as u64 > limit {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.to_string(),
                message: format!(
                    "fixture '{}' is {} bytes (exceeds {} byte limit)",
                    file,
                    bytes.len(),
                    limit
                ),
            });
        }

        if is_probably_binary_fixture(&full_path, &bytes) {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.to_string(),
                message: format!(
                    "fixture '{}' appears to be binary; runners and graders may not handle it well",
                    file
                ),
            });
        }
    }

    warnings
}

pub fn is_probably_binary_fixture(path: &Path, bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(FIXTURE_BINARY_SAMPLE_BYTES)];
    if sample.contains(&0) {
        return true;
    }

    if let Some(mime) = guess_mime_type(path) {
        return !(mime.starts_with("text/")
            || matches!(
                mime.as_str(),
                "application/json" | "application/xml" | "application/yaml" | "application/x-ndjson"
            ));
    }

    std::str::from_utf8(sample).is_err()
}

pub fn missing_expected_output_warnings(eval: &EvalCase, outputs_dir: &Path) -> Vec<String> {
    let Some(expected_files) = eval.expected_output_files.as_ref() else {
        return Vec::new();
    };

    expected_files
        .iter()
        .filter(|relative| !outputs_dir.join(relative).is_file())
        .map(|relative| format!("expected output file '{}' is missing under outputs/", relative))
        .collect()
}

pub fn print_eval_lint_warnings(warnings: &[EvalLintWarning]) {
    for warning in warnings {
        eprintln!("eval lint warning ({}): {}", warning.eval_id, warning.message);
    }
}

pub fn check_eval_suite(
    fs: &impl FileSystem,
    skill_path: &Path,
    expected_skill_name: &str,
    options: EvalCheckOptions,
) -> Result<EvalCheckReport> {
    let suite_path = skill_path.join(EVAL_SUITE_DIR_NAME).join(EVAL_SUITE_MANIFEST_NAME);
    let content = fs.read_to_string(&suite_path)?;
    let suite = parse_eval_suite(&content)?;

    let mut errors = ValidationErrors::new();
    let mut file_count = 0;
    let mut assertion_count = 0;

    if suite.skill_name.as_str() != expected_skill_name {
        errors.push(ValidationError::for_field(
            "skill_name",
            format!(
                "'{}' must match skill frontmatter name '{}'",
                suite.skill_name, expected_skill_name
            ),
        ));
    }

    for eval in &suite.evals {
        let label = format!("evals id '{}'", eval.id);

        if options.require_assertions && !eval.has_checks() {
            errors.push(ValidationError::for_field(
                label.clone(),
                "must define at least one assertion or grader",
            ));
        }

        for file in &eval.files {
            file_count += 1;
            let full_path = skill_path.join(file.as_path());
            if !fs.exists(&full_path) {
                errors.push(ValidationError::for_field(
                    format!("{}.files", label),
                    format!("path '{}' does not exist", full_path.display()),
                ));
                continue;
            }

            if fs.is_file(&full_path) {
                let limit = effective_max_fixture_bytes(options.max_fixture_bytes);
                match fs.read_bytes(&full_path) {
                    Ok(bytes) if bytes.len() as u64 > limit => {
                        errors.push(ValidationError::for_field(
                            format!("{}.files", label),
                            format!(
                                "path '{}' is {} bytes (exceeds {} byte limit)",
                                full_path.display(),
                                bytes.len(),
                                limit
                            ),
                        ));
                    }
                    Err(source) => {
                        errors.push(ValidationError::for_field(
                            format!("{}.files", label),
                            format!("path '{}': {source}", full_path.display()),
                        ));
                    }
                    Ok(_) => {}
                }
            }
        }

        assertion_count += eval.assertions.len();
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    Ok(EvalCheckReport {
        skill_name: suite.skill_name.as_str().to_string(),
        eval_count: suite.evals.len(),
        file_count,
        assertion_count,
        workspace: None,
    })
}

pub fn check_workspace(workspace_path: &Path, options: WorkspaceCheckOptions) -> Result<WorkspaceCheckReport> {
    if !workspace_path.exists() {
        return Err(EvalError::Validation(
            ValidationError::for_field(format!("workspace '{}'", workspace_path.display()), "does not exist").into(),
        ));
    }

    if !workspace_path.is_dir() {
        return Err(EvalError::Validation(
            ValidationError::for_field(
                format!("workspace '{}'", workspace_path.display()),
                "must be a directory",
            )
            .into(),
        ));
    }

    let search_root = ArtifactSearchRoot::for_workspace(workspace_path);
    let mut grading_files = Vec::new();
    let mut timing_files = Vec::new();
    collect_named_files(search_root.as_path(), "grading.json", &mut grading_files)?;
    collect_named_files(search_root.as_path(), "timing.json", &mut timing_files)?;

    let mut errors = ValidationErrors::new();
    let mut assertion_results = 0;
    let mut passed_assertions = 0;
    let mut failed_assertions = 0;
    let mut unsupported_assertions = 0;
    let mut excluded_assertions = 0;

    if options.require_grading && grading_files.is_empty() {
        errors.push(ValidationError::for_field(
            format!("workspace '{}'", workspace_path.display()),
            "must contain at least one grading.json",
        ));
    }

    for grading_path in &grading_files {
        let grading = read_grading_file(grading_path)?;
        validate_grading_file(grading_path, &grading, options, &mut errors);

        let counts = grading::GradingCounts::tally(&grading.assertion_results);
        assertion_results += counts.scored();
        passed_assertions += counts.passed;
        failed_assertions += counts.failed;
        unsupported_assertions += counts.unsupported;
        excluded_assertions += counts.excluded;
    }

    for timing_path in &timing_files {
        let timing = read_timing_file(timing_path)?;
        validate_timing_file(timing_path, &timing, &mut errors);
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    let pass_rate = if assertion_results == 0 {
        None
    } else {
        Some(passed_assertions as f64 / assertion_results as f64)
    };

    Ok(WorkspaceCheckReport {
        grading_files: grading_files.len(),
        timing_files: timing_files.len(),
        assertion_results,
        passed_assertions,
        failed_assertions,
        unsupported_assertions,
        excluded_assertions,
        pass_rate,
    })
}

fn validate_non_empty(value: &str, field: &str, errors: &mut ValidationErrors) {
    if value.trim().is_empty() {
        errors.push(ValidationError::for_field(field, "must be a non-empty string"));
    }
}

/// Where verification looks for the companion artifacts of a run.
///
/// `eval grade` writes `grading.json` into the run directory rather than the
/// workspace, because the verdict is about the run and not about the files the
/// agent happened to leave behind. Pointing `verify` at the workspace is the
/// documented habit, so a workspace path widens to its run directory instead of
/// reporting a graded run as ungraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSearchRoot(PathBuf);

impl ArtifactSearchRoot {
    const WORKSPACE_DIR_NAME: &'static str = "workspace";

    pub fn for_workspace(path: &Path) -> Self {
        if path.file_name().and_then(|name| name.to_str()) == Some(Self::WORKSPACE_DIR_NAME) {
            if let Some(run_dir) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                return Self(run_dir.to_path_buf());
            }
        }
        Self(path.to_path_buf())
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

pub fn collect_named_files(root: &Path, file_name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
    walk_named_files(root, file_name, matches)?;
    matches.sort();
    Ok(())
}

fn walk_named_files(root: &Path, file_name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            walk_named_files(&path, file_name, matches)?;
        } else if file_type.is_file() && path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            matches.push(path);
        }
    }

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

fn validate_grading_file(
    path: &Path,
    grading: &GradingFile,
    options: WorkspaceCheckOptions,
    errors: &mut ValidationErrors,
) {
    let file_label = path.display().to_string();

    if grading.assertion_results.is_empty() {
        errors.push(ValidationError::for_field(
            format!("{} assertion_results", file_label),
            "must contain at least one result",
        ));
    }

    let counts = grading::GradingCounts::tally(&grading.assertion_results);
    let passed = counts.passed;
    let failed = counts.failed;

    for (index, result) in grading.assertion_results.iter().enumerate() {
        validate_non_empty(
            &result.assertion,
            &format!("{} assertion_results[{index}].assertion", file_label),
            errors,
        );
        validate_non_empty(
            &result.evidence,
            &format!("{} assertion_results[{index}].evidence", file_label),
            errors,
        );
        if result.passed && result.is_scored() && grading::evidence_is_trivial(&result.assertion, &result.evidence) {
            errors.push(ValidationError::for_field(
                format!("{} assertion_results[{index}].evidence", file_label),
                "passed assertions must include non-trivial evidence",
            ));
        }
    }

    if grading.summary.passed != passed {
        errors.push(ValidationError::for_field(
            format!("{} summary.passed", file_label),
            format!(
                "{} does not match {} passed assertion results",
                grading.summary.passed, passed
            ),
        ));
    }

    if grading.summary.failed != failed {
        errors.push(ValidationError::for_field(
            format!("{} summary.failed", file_label),
            format!(
                "{} does not match {} failed assertion results",
                grading.summary.failed, failed
            ),
        ));
    }

    if grading.summary.total != grading.assertion_results.len() {
        errors.push(ValidationError::for_field(
            format!("{} summary.total", file_label),
            format!(
                "{} does not match {} assertion results",
                grading.summary.total,
                grading.assertion_results.len()
            ),
        ));
    }

    if grading.summary.unsupported != counts.unsupported {
        errors.push(ValidationError::for_field(
            format!("{} summary.unsupported", file_label),
            format!(
                "{} does not match {} unsupported assertion results",
                grading.summary.unsupported, counts.unsupported
            ),
        ));
    }

    if grading.summary.excluded != counts.excluded {
        errors.push(ValidationError::for_field(
            format!("{} summary.excluded", file_label),
            format!(
                "{} does not match {} excluded assertion results",
                grading.summary.excluded, counts.excluded
            ),
        ));
    }

    if !grading::pass_rate_matches(grading.summary.pass_rate, counts.pass_rate()) {
        errors.push(ValidationError::for_field(
            format!("{} summary.pass_rate", file_label),
            format!(
                "{} does not match computed pass rate {}",
                grading::describe_pass_rate(grading.summary.pass_rate),
                grading::describe_pass_rate(counts.pass_rate())
            ),
        ));
    }

    if options.fail_on_failed_assertions && failed > 0 {
        errors.push(ValidationError::for_field(
            file_label,
            format!("has {} failed assertion result(s)", failed),
        ));
    }
}

fn validate_timing_file(path: &Path, timing: &TimingFile, errors: &mut ValidationErrors) {
    let file_label = path.display().to_string();

    if matches!(timing.total_tokens, Some(0)) {
        errors.push(ValidationError::for_field(
            format!("{} total_tokens", file_label),
            "must be greater than zero when present",
        ));
    }

    if timing.duration_ms == 0 {
        errors.push(ValidationError::for_field(
            format!("{} duration_ms", file_label),
            "must be greater than zero",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::runner::{write_timing_file, EvalRunOutcome, RunStatus};
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
                ..EvalCheckOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.skill_name, "csv-analyzer");
        assert_eq!(report.eval_count, 1);
        assert_eq!(report.file_count, 1);
        assert_eq!(report.assertion_count, 1);
    }

    #[test]
    fn check_eval_suite_rejects_fixture_exceeding_byte_limit() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/csv-analyzer/evals/evals.json"),
            r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "large-file",
      "prompt": "Analyze evals/files/sales.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/sales.csv"]
    }
  ]
}"#,
        );
        fs.insert(
            Path::new("/csv-analyzer/evals/files/sales.csv"),
            "month,revenue\nMay,10",
        );

        let err = check_eval_suite(
            &fs,
            Path::new("/csv-analyzer"),
            "csv-analyzer",
            EvalCheckOptions {
                max_fixture_bytes: Some(10),
                ..EvalCheckOptions::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("exceeds 10 byte limit"));
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
                ..EvalCheckOptions::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("must define at least one assertion"));
    }

    #[test]
    fn verifying_a_workspace_path_sees_the_grading_written_beside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("runs").join("run-001");
        let workspace = run_dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            run_dir.join("grading.json"),
            r#"{
  "assertion_results": [
    {
      "assertion": "file 'summary.md' exists",
      "passed": true,
      "evidence": "'summary.md' exists and holds 20 bytes",
      "grader": { "kind": "declarative" }
    }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 1, "unsupported": 0, "pass_rate": 1.0 }
}"#,
        )
        .unwrap();

        let report = check_workspace(
            &workspace,
            WorkspaceCheckOptions {
                require_grading: true,
                fail_on_failed_assertions: true,
            },
        )
        .unwrap();

        assert_eq!(report.grading_files, 1);
        assert_eq!(report.passed_assertions, 1);
    }

    #[test]
    fn a_directory_that_is_not_a_workspace_is_searched_as_given() {
        let tmp = tempfile::tempdir().unwrap();
        let sibling = tmp.path().join("sibling");
        let target = tmp.path().join("target");
        fs::create_dir_all(&sibling).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(sibling.join("grading.json"), "{}").unwrap();

        let root = ArtifactSearchRoot::for_workspace(&target);

        assert_eq!(root.as_path(), target.as_path());
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
    {
      "assertion": "Includes a summary",
      "passed": true,
      "evidence": "summary.md exists in outputs",
      "grader": { "kind": "mechanical" }
    },
    {
      "assertion": "Includes a chart",
      "passed": false,
      "evidence": "no chart file found in outputs",
      "grader": { "kind": "mechanical" }
    }
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
        assert_eq!(report.pass_rate, Some(0.5));
    }

    #[test]
    fn read_timing_file_accepts_runner_written_timing_file() {
        let tmp = tempdir().unwrap();
        let timing_path = tmp.path().join("run").join("timing.json");
        let outcome = EvalRunOutcome {
            status: RunStatus::Completed,
            failure_kind: None,
            duration_ms: 2500,
            exit_code: Some(0),
            total_tokens: Some(1000),
            input_tokens: Some(700),
            output_tokens: Some(300),
            cost_usd: Some(0.42),
            final_text: "done".to_string(),
        };

        write_timing_file(&timing_path, &outcome).unwrap();
        let timing = read_timing_file(&timing_path).unwrap();

        assert_eq!(timing.duration_ms, 2500);
        assert_eq!(timing.exit_code, Some(0));
        assert_eq!(timing.total_tokens, Some(1000));
        assert_eq!(timing.input_tokens, Some(700));
        assert_eq!(timing.output_tokens, Some(300));
        assert_eq!(timing.cost_usd, Some(0.42));
    }

    #[test]
    fn read_timing_file_rejects_unknown_fields() {
        let tmp = tempdir().unwrap();
        let timing_path = tmp.path().join("timing.json");
        fs::write(&timing_path, r#"{ "duration_ms": 2500, "wall_clock_ms": 2500 }"#).unwrap();

        let error = read_timing_file(&timing_path).unwrap_err();

        assert!(error.to_string().contains("wall_clock_ms"));
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
    {
      "assertion": "Includes a summary",
      "passed": false,
      "evidence": "summary.md missing from outputs",
      "grader": { "kind": "mechanical" }
    }
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

    fn sample_suite_with_eval(eval: EvalCase) -> EvalSuite {
        EvalSuite {
            schema_version: 1,
            skill_name: NonEmptyString("demo-skill".to_string()),
            evals: vec![eval],
        }
    }

    fn sample_eval_case(id: &str, prompt: &str, expected_output: &str) -> EvalCase {
        EvalCase {
            id: EvalCaseId(id.to_string()),
            name: None,
            description: None,
            prompt: NonEmptyString(prompt.to_string()),
            expected_output: NonEmptyString(expected_output.to_string()),
            files: vec![],
            assertions: vec![NonEmptyString("checks something".to_string())],
            tags: None,
            priority: None,
            timeout_secs: None,
            expected_output_files: None,
            grader_hints: None,
            graders: vec![],
            skill_disclosure: SkillDisclosure::default(),
            scaffold: None,
        }
    }

    #[test]
    fn lint_eval_suite_warns_on_vague_prompt() {
        let suite = sample_suite_with_eval(sample_eval_case("one", "too short", "long enough output"));

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings.iter().any(|warning| warning.message.contains("too vague")));
    }

    #[test]
    fn lint_eval_suite_warns_on_generic_expected_output() {
        let suite = sample_suite_with_eval(sample_eval_case("one", "A sufficiently long prompt here", "short"));

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings.iter().any(|warning| warning.message.contains("too generic")));
    }

    #[test]
    fn lint_eval_suite_warns_when_fixtures_are_not_referenced() {
        let mut eval = sample_eval_case(
            "one",
            "Analyze the attached data without paths",
            "A detailed analysis output",
        );
        eval.files = vec![RelativeSkillPath("evals/files/input.csv".to_string())];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("does not reference a file path")));
    }

    #[test]
    fn lint_eval_suite_warns_on_duplicate_fixture_paths() {
        let mut eval = sample_eval_case(
            "one",
            "Analyze evals/files/input.csv twice",
            "A detailed analysis output",
        );
        eval.files = vec![
            RelativeSkillPath("evals/files/input.csv".to_string()),
            RelativeSkillPath("evals/files/input.csv".to_string()),
        ];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("duplicate fixture path")));
    }

    #[test]
    fn lint_eval_suite_warns_on_empty_assertions_by_default() {
        let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
        eval.assertions = vec![];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("neither assertions nor graders")));
    }

    #[test]
    fn lint_eval_suite_does_not_warn_when_only_typed_graders_are_declared() {
        let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
        eval.assertions = vec![];
        eval.graders = vec![serde_json::from_value(serde_json::json!({
            "type": "file_exists",
            "path": "summary.md"
        }))
        .unwrap()];
        let suite = sample_suite_with_eval(eval);

        assert!(lint_eval_suite(&suite, EvalLintOptions::default()).is_empty());
    }

    #[test]
    fn lint_eval_suite_allows_empty_assertions_when_requested() {
        let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
        eval.assertions = vec![];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite(
            &suite,
            EvalLintOptions {
                allow_empty_assertions: true,
                ..EvalLintOptions::default()
            },
        );
        assert!(!warnings
            .iter()
            .any(|warning| warning.message.contains("neither assertions nor graders")));
    }

    /// The contradiction this lint exists to catch: a case that checks whether the
    /// run reaches for the skill, in a prompt that hands it the skill.
    #[test]
    fn a_case_that_checks_triggering_while_announcing_the_skill_is_linted() {
        let eval: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "triggers",
            "prompt": "Turn the staged sales file into a summary for the finance team.",
            "expected_output": "The run reaches for the skill rather than improvising.",
            "graders": [{ "type": "skill_used" }],
        }))
        .unwrap();
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("skill_disclosure")));
    }

    #[test]
    fn the_scaffolds_triggering_case_can_measure_triggering() {
        let suite = scaffold_eval_suite("demo-skill");
        let triggering = suite
            .evals
            .iter()
            .find(|eval| eval.id.as_str() == "triggers-the-skill")
            .expect("the scaffold declares a triggering case");

        assert_eq!(triggering.skill_disclosure, SkillDisclosure::Unannounced);
        assert!(triggering.checks_skill_engagement());
        assert!(!triggering.prompt.as_str().contains("demo-skill"));

        let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.message.contains("skill_disclosure")),
            "the shipped scaffold must not trip its own lint: {warnings:?}"
        );
    }

    #[test]
    fn scaffold_eval_suite_round_trips_through_serde_json() {
        let suite = scaffold_eval_suite("demo-skill");
        let json = serde_json::to_string_pretty(&suite).unwrap();
        let parsed = parse_eval_suite(&json).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.skill_name.as_str(), "demo-skill");
        assert_eq!(parsed.evals.len(), 2);
        assert_eq!(parsed.evals[0].id.as_str(), "produces-a-summary");
        assert_eq!(parsed.evals[1].id.as_str(), "triggers-the-skill");
        assert!(parsed.evals.iter().all(EvalCase::has_checks));
    }

    #[test]
    fn scaffold_declares_only_typed_graders_so_nothing_needs_string_sniffing() {
        let suite = scaffold_eval_suite("demo-skill");
        assert!(suite.evals.iter().all(|eval| eval.assertions.is_empty()));
        assert!(suite.evals.iter().all(|eval| !eval.graders.is_empty()));
    }

    #[test]
    fn a_manifest_that_omits_schema_version_still_parses_documented_fields() {
        let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "tags": ["smoke"],
      "grader_hints": { "hint": "value" },
      "graders": [{ "type": "skill_used" }]
    }
  ]
}"#;

        let suite = parse_eval_suite(json).expect("a manifest without schema_version must still parse");
        assert_eq!(suite.schema_version, 1);
        assert_eq!(suite.evals[0].tags.as_deref(), Some(&["smoke".to_string()][..]));
        assert!(suite.evals[0].grader_hints.is_some());
        assert!(!suite.evals[0].graders.is_empty());
    }

    #[test]
    fn scaffold_eval_suite_passes_strict_check() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/demo-skill/SKILL.md"),
            "---\nname: demo-skill\ndescription: demo\n---\n",
        );
        write_eval_suite(&fs, Path::new("/demo-skill"), &scaffold_eval_suite("demo-skill")).unwrap();

        check_eval_suite(
            &fs,
            Path::new("/demo-skill"),
            "demo-skill",
            EvalCheckOptions {
                require_assertions: true,
                ..EvalCheckOptions::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn parse_v2_manifest_with_all_metadata_fields_round_trips() {
        let json = r#"{
  "schema_version": 2,
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "full",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "files": ["evals/files/input.csv"],
      "assertions": ["checks output"],
      "tags": ["smoke", "regression"],
      "priority": "critical",
      "timeout_secs": 90,
      "expected_output_files": ["report.md", "summary.md"],
      "grader_hints": { "strict_json": true, "threshold": 0.8 }
    }
  ]
}"#;

        let suite = parse_eval_suite(json).unwrap();
        let round_trip = serde_json::to_string(&suite).unwrap();
        let reparsed = parse_eval_suite(&round_trip).unwrap();
        let eval = &reparsed.evals[0];
        assert_eq!(reparsed.schema_version, 2);
        assert_eq!(
            eval.tags.as_deref(),
            Some(["smoke".to_string(), "regression".to_string()].as_slice())
        );
        assert_eq!(eval.priority, Some(EvalPriority::Critical));
        assert_eq!(eval.timeout_secs, Some(90));
        assert_eq!(
            eval.expected_output_files.as_deref(),
            Some(["report.md".to_string(), "summary.md".to_string()].as_slice())
        );
        assert_eq!(
            eval.grader_hints
                .as_ref()
                .and_then(|hints| hints.get("strict_json"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn a_manifest_without_schema_version_parses_case_metadata_fields() {
        let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "tags": ["smoke"]
    }
  ]
}"#;

        let suite = parse_eval_suite(json).expect("tags must parse without a declared schema_version");
        assert_eq!(suite.evals[0].tags.as_deref(), Some(&["smoke".to_string()][..]));
    }

    #[test]
    fn parse_v2_manifest_rejects_unknown_suite_fields() {
        let json = r#"{
  "schema_version": 2,
  "skill_name": "demo-skill",
  "future_suite_field": true,
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err();
        assert!(err.to_string().contains("future_suite_field"));
    }

    #[test]
    fn parse_rejects_a_misspelled_case_field_regardless_of_schema_version() {
        let json = r#"{
  "schema_version": 2,
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "assertion": [
        "The response follows the skill instructions"
      ]
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err().to_string();
        assert!(err.contains("unknown field `assertion`"), "{err}");
        assert!(err.contains("expected one of"), "{err}");
    }

    #[test]
    fn parse_current_version_manifest_rejects_unknown_suite_fields() {
        let json = r#"{
  "schema_version": 3,
  "skill_name": "demo-skill",
  "future_suite_field": true,
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err().to_string();
        assert!(err.contains("future_suite_field"), "{err}");
    }

    #[test]
    fn parse_current_version_manifest_rejects_misspelled_case_fields() {
        let json = r#"{
  "schema_version": 3,
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    },
    {
      "id": "two",
      "prompt": "Another sufficiently long prompt",
      "expected_output": "Another detailed analysis output",
      "grader": [
        { "type": "skill_used" }
      ]
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err().to_string();
        assert!(err.contains("grader"), "{err}");
        assert!(err.contains("evals[1]"), "{err}");
    }

    #[test]
    fn parse_accepts_any_schema_version_value_as_inert() {
        let below_minimum = r#"{
  "schema_version": 0,
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;
        let suite = parse_eval_suite(below_minimum).expect("schema_version 0 must no longer be rejected");
        assert_eq!(suite.schema_version, 0);

        let far_beyond_any_release = r#"{
  "schema_version": 99,
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;
        let suite = parse_eval_suite(far_beyond_any_release).expect("schema_version 99 must no longer be rejected");
        assert_eq!(suite.schema_version, 99);
    }

    #[test]
    fn effective_timeout_secs_prefers_per_eval_override() {
        let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
        eval.timeout_secs = Some(42);
        assert_eq!(effective_timeout_secs(&eval, Some(99)), Some(42));
        eval.timeout_secs = None;
        assert_eq!(effective_timeout_secs(&eval, Some(99)), Some(99));
        assert_eq!(effective_timeout_secs(&eval, None), None);
    }

    #[test]
    fn lint_eval_suite_warns_on_large_fixture() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        let fixture_path = skill_path.join("evals/files/large.csv");
        std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture_path, vec![b'a'; DEFAULT_MAX_FIXTURE_BYTES as usize + 1]).unwrap();

        let mut eval = sample_eval_case(
            "one",
            "Analyze evals/files/large.csv carefully",
            "A detailed analysis output",
        );
        eval.files = vec![RelativeSkillPath("evals/files/large.csv".to_string())];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
        assert!(warnings.iter().any(|warning| warning.message.contains("exceeds")));
    }

    #[test]
    fn lint_eval_suite_does_not_warn_on_small_fixture() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        let fixture_path = skill_path.join("evals/files/small.csv");
        std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture_path, "month,revenue\nMay,10").unwrap();

        let mut eval = sample_eval_case(
            "one",
            "Analyze evals/files/small.csv carefully",
            "A detailed analysis output",
        );
        eval.files = vec![RelativeSkillPath("evals/files/small.csv".to_string())];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
        assert!(!warnings.iter().any(|warning| warning.message.contains("exceeds")));
    }

    #[test]
    fn lint_eval_suite_warns_on_binary_fixture() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        let fixture_path = skill_path.join("evals/files/binary.bin");
        std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture_path, b"text\0binary").unwrap();

        let mut eval = sample_eval_case(
            "one",
            "Analyze evals/files/binary.bin carefully",
            "A detailed analysis output",
        );
        eval.files = vec![RelativeSkillPath("evals/files/binary.bin".to_string())];
        let suite = sample_suite_with_eval(eval);

        let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
        assert!(warnings.iter().any(|warning| warning.message.contains("binary")));
    }

    #[test]
    fn missing_expected_output_warnings_lists_missing_files() {
        let temp = tempdir().unwrap();
        let outputs = temp.path().join("outputs");
        std::fs::create_dir_all(&outputs).unwrap();
        std::fs::write(outputs.join("present.md"), "ok").unwrap();

        let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
        eval.expected_output_files = Some(vec!["present.md".to_string(), "missing.md".to_string()]);

        let warnings = missing_expected_output_warnings(&eval, &outputs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.md"));
    }

    #[test]
    fn a_case_that_declares_two_graders_with_the_same_name_is_refused() {
        let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "graders": [
        { "type": "skill_used", "name": "primary" },
        { "type": "contains", "text": "total", "name": "primary" }
      ]
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err().to_string();
        assert!(err.contains("duplicate name 'primary'"));
    }

    #[test]
    fn a_case_with_a_name_and_description_round_trips_through_serde_json() {
        let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "name": "Readable case title",
      "description": "What this case is checking for.",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

        let suite = parse_eval_suite(json).unwrap();
        let round_tripped: EvalSuite = parse_eval_suite(&serde_json::to_string(&suite).unwrap()).unwrap();

        assert_eq!(
            round_tripped.evals[0].name.as_ref().map(NonEmptyString::to_string),
            Some("Readable case title".to_string())
        );
        assert_eq!(
            round_tripped.evals[0]
                .description
                .as_ref()
                .map(NonEmptyString::to_string),
            Some("What this case is checking for.".to_string())
        );
    }

    #[test]
    fn a_case_with_an_empty_name_is_refused() {
        let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "name": "",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

        let err = parse_eval_suite(json).unwrap_err();
        assert!(err.to_string().contains("must be a non-empty string"));
    }

    #[test]
    fn a_case_without_a_name_or_description_omits_them_from_serialized_json() {
        let suite = sample_suite_with_eval(sample_eval_case(
            "one",
            "A sufficiently long prompt here",
            "A detailed analysis output",
        ));

        let json = serde_json::to_string(&suite.evals[0]).unwrap();
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("\"description\""));
    }
}
