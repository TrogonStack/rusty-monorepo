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

use super::evals::{parse_eval_suite, EvalCase, EvalError, EvalSuite, Result};
use super::report::{ReportDocument, RunRecord};
use super::validation::{ValidationError, ValidationErrors};

pub const GRADING_SCHEMA_VERSION: &str = "trg.skills-eval.grading.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraderKind {
    Mechanical,
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
    pub assertion: String,
    pub passed: bool,
    pub evidence: String,
    pub grader: GraderInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub pass_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingFile {
    pub schema_version: String,
    pub assertion_results: Vec<AssertionGradeResult>,
    pub summary: GradingSummary,
}

#[derive(Debug, Clone, Default)]
pub struct GradeOptions {
    pub grader: GraderMode,
    pub grader_model: Option<String>,
    pub grader_command: Option<String>,
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GradeReport {
    pub runs_graded: usize,
    pub assertions_graded: usize,
    pub passed: usize,
    pub failed: usize,
    pub needs_llm: usize,
}

#[derive(Debug, Clone)]
struct RunContext {
    run_dir: PathBuf,
    workspace_dir: PathBuf,
    outputs_dir: PathBuf,
    transcript_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) enum MechanicalKind {
    FileExists { path: String },
    FileCount { count: usize, dir: Option<String> },
    ValidJson { path: Option<String> },
    ValidCsv { path: Option<String> },
    ValidMarkdownHeadings { path: Option<String> },
    ImageExists { path: String },
    ImageDimensions { path: String, width: u32, height: u32 },
    ContainsString { needle: String, path: Option<String> },
    MatchesRegex { pattern: String, path: Option<String> },
    RowCount { count: usize, path: Option<String> },
    SchemaValidation { schema: String, path: Option<String> },
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
    let evals_path = skill_path.join("evals").join("evals.json");
    let evals_content = std::fs::read_to_string(&evals_path).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                format!("evals '{}'", evals_path.display()),
                format!("failed to read: {e}"),
            )
            .into(),
        )
    })?;
    let suite: EvalSuite = parse_eval_suite(&evals_content)?;

    let case_index: HashMap<String, &EvalCase> = suite.evals.iter().map(|c| (c.id.to_string(), c)).collect();

    let grader_config = build_grader_config(&options);
    if document.dimensions.graders.is_empty() {
        document.dimensions.graders.push(grader_config.clone());
    }

    let mut report = GradeReport {
        runs_graded: 0,
        assertions_graded: 0,
        passed: 0,
        failed: 0,
        needs_llm: 0,
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

        let ctx = run_context(report_dir, run);
        let mut assertion_results = Vec::with_capacity(case.assertions.len());

        for assertion in &case.assertions {
            let result = grade_assertion(assertion.as_str(), case, &ctx, &options)?;
            if result.grader.kind == GraderKind::NeedsLlm {
                report.needs_llm += 1;
            }
            if result.passed {
                report.passed += 1;
            } else {
                report.failed += 1;
            }
            report.assertions_graded += 1;
            assertion_results.push(result);
        }

        let grading = build_grading_file(assertion_results)?;
        validate_grading_document(&grading, options.strict)?;

        let grading_path = ctx.run_dir.join("grading.json");
        std::fs::write(&grading_path, serde_json::to_string_pretty(&grading)?)?;

        store_grader_artifacts(run_mut, report_dir, &ctx, &options, &grading)?;

        report.runs_graded += 1;
    }

    update_report_after_grading(&mut document, &runs, report_dir, &grader_config)?;
    std::fs::write(report_dir.join("report.json"), serde_json::to_string_pretty(&document)?)?;

    Ok(report)
}

fn run_context(report_dir: &Path, run: &RunRecord) -> RunContext {
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
    }
}

fn build_grader_config(options: &GradeOptions) -> serde_json::Value {
    serde_json::json!({
        "mode": format!("{:?}", options.grader).to_lowercase(),
        "model": options.grader_model,
        "command": options.grader_command,
        "strict": options.strict,
    })
}

fn grade_assertion(
    assertion: &str,
    eval_case: &EvalCase,
    ctx: &RunContext,
    options: &GradeOptions,
) -> Result<AssertionGradeResult> {
    match options.grader {
        GraderMode::Script => grade_with_script(assertion, eval_case, ctx, options),
        GraderMode::Llm => grade_with_llm(assertion, ctx, options),
        GraderMode::None | GraderMode::Auto => {
            if let Some(kind) = parse_mechanical_kind(assertion) {
                let (passed, evidence) = evaluate_mechanical(&kind, ctx)?;
                Ok(AssertionGradeResult {
                    assertion: assertion.to_string(),
                    passed,
                    evidence,
                    grader: GraderInfo {
                        kind: GraderKind::Mechanical,
                        model: None,
                        command: None,
                    },
                    rationale: None,
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

fn needs_llm_result(assertion: &str, evidence: &str) -> AssertionGradeResult {
    AssertionGradeResult {
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.to_string(),
        grader: GraderInfo {
            kind: GraderKind::NeedsLlm,
            model: None,
            command: None,
        },
        rationale: None,
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

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(serde_json::to_string(&input)?.as_bytes())
            .map_err(EvalError::Io)?;
    }

    let output = child.wait_with_output().map_err(EvalError::Io)?;
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
        return Ok(AssertionGradeResult {
            assertion: assertion.to_string(),
            passed: false,
            evidence: format!(
                "script grader exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            grader: GraderInfo {
                kind: GraderKind::Script,
                model: None,
                command: Some(command.to_string()),
            },
            rationale: None,
        });
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
        assertion: assertion.to_string(),
        passed: parsed.passed,
        evidence: parsed.evidence,
        grader: GraderInfo {
            kind: GraderKind::Script,
            model: None,
            command: Some(command.to_string()),
        },
        rationale: parsed.rationale,
    })
}

#[derive(Debug, Deserialize)]
struct ScriptGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

fn grade_with_llm(assertion: &str, ctx: &RunContext, options: &GradeOptions) -> Result<AssertionGradeResult> {
    let model = options.grader_model.as_deref().ok_or_else(|| {
        EvalError::Validation(ValidationError::for_field("--grader-model", "is required when --grader llm").into())
    })?;

    if let Some(kind) = parse_mechanical_kind(assertion) {
        let (passed, evidence) = evaluate_mechanical(&kind, ctx)?;
        return Ok(AssertionGradeResult {
            assertion: assertion.to_string(),
            passed,
            evidence,
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
        });
    }

    Err(EvalError::Validation(
        ValidationError::for_field(
            "--grader llm",
            format!(
                "LLM grading for assertion '{}' requires API integration (model: {model})",
                assertion
            ),
        )
        .into(),
    ))
}

pub(crate) fn parse_mechanical_kind(assertion: &str) -> Option<MechanicalKind> {
    let lower = assertion.trim().to_lowercase();

    if let Some(path) = extract_quoted_or_token_after(&lower, "file ", " exists") {
        return Some(MechanicalKind::FileExists { path });
    }
    if lower.ends_with(" exists") && !lower.contains("file count") && !lower.contains("image ") {
        let path = lower.trim_end_matches(" exists").trim().to_string();
        if !path.is_empty() && !path.contains(' ') {
            return Some(MechanicalKind::FileExists { path });
        }
    }

    if let Some(count) = extract_usize_after(&lower, "file count is ") {
        return Some(MechanicalKind::FileCount { count, dir: None });
    }
    if let Some(count) = extract_usize_before(&lower, " files") {
        let dir = extract_after(&lower, " in ").map(str::to_string);
        return Some(MechanicalKind::FileCount { count, dir });
    }
    if let Some(count) = extract_usize_after(&lower, "contains ") {
        if lower.contains(" files") {
            return Some(MechanicalKind::FileCount { count, dir: None });
        }
    }

    if lower.contains("valid json") || lower.contains("is valid json") {
        let path = extract_path_before(&lower, " is valid json").or_else(|| extract_path_before(&lower, "valid json"));
        return Some(MechanicalKind::ValidJson { path });
    }

    if lower.contains("valid csv") || lower.contains("is valid csv") {
        let path = extract_path_before(&lower, " is valid csv").or_else(|| extract_path_before(&lower, "valid csv"));
        return Some(MechanicalKind::ValidCsv { path });
    }

    if lower.contains("markdown headings") || lower.contains("valid markdown") {
        let path = extract_path_before(&lower, " has valid markdown headings");
        return Some(MechanicalKind::ValidMarkdownHeadings { path });
    }

    if let Some(path) = extract_after(&lower, "image exists at ") {
        return Some(MechanicalKind::ImageExists { path: path.to_string() });
    }
    if lower.starts_with("image ") && lower.ends_with(" exists") {
        let path = lower
            .trim_start_matches("image ")
            .trim_end_matches(" exists")
            .trim()
            .to_string();
        return Some(MechanicalKind::ImageExists { path });
    }

    if let Some(dims) = extract_dimensions(&lower) {
        if let Some(path) = extract_path_before(&lower, " is ") {
            return Some(MechanicalKind::ImageDimensions {
                path: path.to_string(),
                width: dims.0,
                height: dims.1,
            });
        }
        if lower.contains("image dimensions are ") {
            return Some(MechanicalKind::ImageDimensions {
                path: "outputs".to_string(),
                width: dims.0,
                height: dims.1,
            });
        }
    }

    if let Some(needle) = extract_quoted(assertion, "contains ") {
        let path = extract_after(&lower, " in ").map(str::to_string);
        return Some(MechanicalKind::ContainsString { needle, path });
    }
    if let Some(needle) = extract_quoted(assertion, "includes ") {
        let path = extract_after(&lower, " in ").map(str::to_string);
        return Some(MechanicalKind::ContainsString { needle, path });
    }
    if lower.starts_with("output includes ") {
        let needle = assertion.trim()[16..].trim().trim_matches('"').to_string();
        return Some(MechanicalKind::ContainsString { needle, path: None });
    }

    if let Some(pattern) = extract_regex_pattern(&lower) {
        let path = extract_path_before(&lower, " matches");
        return Some(MechanicalKind::MatchesRegex { pattern, path });
    }

    if let Some(count) = extract_usize_after(&lower, "row count is ") {
        let path = extract_before(&lower, " row count").map(str::to_string);
        return Some(MechanicalKind::RowCount { count, path });
    }
    if let Some(count) = extract_usize_before(&lower, " rows") {
        let path = extract_before(&lower, " has ")
            .or_else(|| extract_before(&lower, " in "))
            .map(str::to_string);
        return Some(MechanicalKind::RowCount { count, path });
    }

    if lower.contains("schema validation") || lower.contains("validates against schema ") {
        let schema = extract_after(&lower, "validates against schema ")
            .or_else(|| extract_after(&lower, "schema validation for "))
            .unwrap_or("default")
            .trim()
            .to_string();
        let path = extract_after(&lower, " for ").map(str::to_string);
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
            validate_json_file(&target)
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
            let target = path
                .as_ref()
                .map(|p| resolve_path(ctx, p))
                .unwrap_or_else(|| ctx.outputs_dir.join("output.json"));
            let valid = target.is_file()
                && serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&target).unwrap_or_default())
                    .is_ok();
            Ok((
                valid,
                format!(
                    "schema '{schema}' validation on {}: {}",
                    target.display(),
                    if valid {
                        "valid JSON document"
                    } else {
                        "invalid or missing"
                    }
                ),
            ))
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

fn validate_json_file(path: &Path) -> Result<(bool, String)> {
    if !path.is_file() {
        return Ok((false, format!("JSON file not found at {}", path.display())));
    }
    let content = std::fs::read_to_string(path)?;
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(_) => Ok((true, format!("{} is valid JSON", path.display()))),
        Err(e) => Ok((false, format!("{} is invalid JSON: {e}", path.display()))),
    }
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
    let passed = assertion_results.iter().filter(|r| r.passed).count();
    let failed = assertion_results.len() - passed;
    let total = assertion_results.len();
    let pass_rate = if total == 0 { 0.0 } else { passed as f64 / total as f64 };

    Ok(GradingFile {
        schema_version: GRADING_SCHEMA_VERSION.to_string(),
        assertion_results,
        summary: GradingSummary {
            passed,
            failed,
            total,
            pass_rate,
        },
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

    if grading.schema_version != GRADING_SCHEMA_VERSION {
        errors.push(ValidationError::for_field(
            "schema_version",
            format!(
                "expected '{}', got '{}'",
                GRADING_SCHEMA_VERSION, grading.schema_version
            ),
        ));
    }

    if grading.assertion_results.is_empty() {
        errors.push(ValidationError::for_field(
            "assertion_results",
            "must contain at least one result",
        ));
    }

    let passed = grading.assertion_results.iter().filter(|r| r.passed).count();
    let failed = grading.assertion_results.len() - passed;

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
        if result.passed && evidence_is_trivial(&result.assertion, &result.evidence) {
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

    let expected_rate = if grading.summary.total == 0 {
        0.0
    } else {
        passed as f64 / grading.summary.total as f64
    };
    if (grading.summary.pass_rate - expected_rate).abs() > 0.0001 {
        errors.push(ValidationError::for_field(
            "summary.pass_rate",
            format!(
                "{} does not match computed rate {expected_rate}",
                grading.summary.pass_rate
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
            document.assertion_results.push(serde_json::json!({
                "run_id": run.id,
                "eval_case_id": run.eval_case_id,
                "assertion": result.assertion,
                "passed": result.passed,
                "evidence": result.evidence,
                "grader": result.grader,
            }));
        }
    }

    Ok(())
}

fn extract_quoted(source: &str, prefix: &str) -> Option<String> {
    let lower = source.to_lowercase();
    let idx = lower.find(&prefix.to_lowercase())?;
    let rest = &source[idx + prefix.len()..];
    let rest = rest.trim_start();
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

fn extract_quoted_or_token_after(lower: &str, prefix: &str, suffix: &str) -> Option<String> {
    let start = lower.find(prefix)? + prefix.len();
    let end = lower[start..].find(suffix)? + start;
    let path = lower[start..end].trim().trim_matches('"').trim_matches('\'');
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn extract_after<'a>(lower: &'a str, prefix: &str) -> Option<&'a str> {
    let idx = lower.find(prefix)? + prefix.len();
    Some(lower[idx..].trim())
}

fn extract_before<'a>(lower: &'a str, suffix: &str) -> Option<&'a str> {
    let idx = lower.find(suffix)?;
    Some(lower[..idx].trim())
}

fn extract_path_before(lower: &str, suffix: &str) -> Option<String> {
    extract_before(lower, suffix)
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
}

fn extract_usize_after(lower: &str, prefix: &str) -> Option<usize> {
    let rest = extract_after(lower, prefix)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn extract_usize_before(lower: &str, suffix: &str) -> Option<usize> {
    let before = extract_before(lower, suffix)?;
    before.split_whitespace().last()?.parse().ok()
}

fn extract_dimensions(lower: &str) -> Option<(u32, u32)> {
    let re = Regex::new(r"(\d+)\s*[x×]\s*(\d+)").ok()?;
    let caps = re.captures(lower)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?))
}

fn extract_regex_pattern(lower: &str) -> Option<String> {
    if let Some(start) = lower.find('/') {
        if let Some(end) = lower[start + 1..].find('/') {
            return Some(lower[start + 1..start + 1 + end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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
            schema_version: GRADING_SCHEMA_VERSION.to_string(),
            assertion_results: vec![AssertionGradeResult {
                assertion: "file out.json exists".to_string(),
                passed: true,
                evidence: "file out.json exists".to_string(),
                grader: GraderInfo {
                    kind: GraderKind::Mechanical,
                    model: None,
                    command: None,
                },
                rationale: None,
            }],
            summary: GradingSummary {
                passed: 1,
                failed: 0,
                total: 1,
                pass_rate: 1.0,
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let ctx = ctx_with_outputs(tmp.path());
        fs::write(ctx.workspace_dir.join("done.txt"), "ok").unwrap();

        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_model: None,
            grader_command: Some(script.to_string_lossy().into_owned()),
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
}
