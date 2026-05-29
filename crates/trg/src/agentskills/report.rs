use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fs::FileSystem;

use super::cache::RunCacheInfo;
use super::evals::{parse_eval_suite, EvalError, EvalSuite, Result};
use super::feedback::{
    collect_improvement_feedback, feedback_path_for_run, load_run_feedback_entries, summarize_feedback,
    FeedbackDocument, HumanFeedbackSummary, ImprovementFeedbackRecord,
};
use super::layout::{ensure_iteration_available, slugs_for_suite, write_docs_mirror_layout};
use super::outputs::OUTPUTS_DIR;
use super::validation::ValidationError;

pub const SCHEMA_VERSION: &str = "trg.skills-eval.report.v1";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, clap::ValueEnum, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SkillStaging {
    #[value(name = "symlink")]
    Symlink,
    #[value(name = "copy")]
    Copy,
}

impl SkillStaging {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::Copy => "copy",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, clap::ValueEnum, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    #[value(name = "with_skill")]
    WithSkill,
    #[value(name = "without_skill")]
    WithoutSkill,
    #[value(name = "old_skill")]
    OldSkill,
}

impl ScenarioKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WithSkill => "with_skill",
            Self::WithoutSkill => "without_skill",
            Self::OldSkill => "old_skill",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildReportOptions {
    pub report_id: Option<String>,
    pub generated_at: Option<String>,
    pub iteration: Option<u32>,
    pub attempts: u32,
    /// Filesystem path used to read the old skill directory (for hashing).
    pub old_skill_path: Option<PathBuf>,
    /// User-supplied path recorded in report metadata.
    pub user_old_skill_path: Option<PathBuf>,
    /// CLI runner kind when `--runner` is set (e.g. `codex`, `claude-code`).
    pub runner: Option<String>,
    /// Resolved runner binary path from the pre-run availability probe.
    pub runner_binary: Option<String>,
    /// Runner `--version` output captured during the availability probe.
    pub runner_version: Option<String>,
    /// How the skill is staged into each run workspace.
    pub skill_staging: SkillStaging,
}

impl Default for BuildReportOptions {
    fn default() -> Self {
        Self {
            report_id: None,
            generated_at: None,
            iteration: None,
            attempts: 1,
            old_skill_path: None,
            user_old_skill_path: None,
            runner: None,
            runner_binary: None,
            runner_version: None,
            skill_staging: SkillStaging::Symlink,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReportBundle {
    pub report_id: String,
    pub skill_name: String,
    pub document: ReportDocument,
    pub workspace_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportDocument {
    pub schema_version: String,
    pub report: ReportSection,
    pub suite: SuiteSection,
    pub dimensions: DimensionsSection,
    pub runs: Vec<RunRecord>,
    pub assertion_results: Vec<serde_json::Value>,
    pub summaries: SummariesSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub improvement_feedback: Vec<ImprovementFeedbackRecord>,
    pub comparisons: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration_summary: Option<super::benchmark::IterationSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportSection {
    pub id: String,
    pub generated_at: String,
    pub iteration: u32,
    pub producer: ProducerSection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_binary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProducerSection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CiSection {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_attempt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SuiteSection {
    pub skill_name: String,
    pub skill_path: String,
    pub skill_hash: String,
    pub evals_path: String,
    pub evals_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_skill_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_skill_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DimensionsSection {
    pub eval_cases: Vec<EvalCaseDimension>,
    pub assertions: Vec<AssertionDimension>,
    pub skill_revisions: Vec<SkillRevisionDimension>,
    pub model_configs: Vec<ModelConfigDimension>,
    pub scenarios: Vec<ScenarioDimension>,
    pub graders: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalCaseDimension {
    pub id: String,
    pub slug: String,
    pub prompt: String,
    pub expected_output: String,
    pub files: Vec<String>,
    pub assertion_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssertionDimension {
    pub id: String,
    pub eval_case_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillRevisionDimension {
    pub id: String,
    pub skill_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModelConfigDimension {
    pub id: String,
    pub capture_status: String,
    pub label: String,
    pub parameters: serde_json::Map<String, serde_json::Value>,
    pub parameter_sources: serde_json::Map<String, serde_json::Value>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioDimension {
    pub id: ScenarioKind,
    pub kind: ScenarioKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunRecord {
    pub id: String,
    pub eval_case_id: String,
    pub eval_slug: String,
    pub scenario_id: ScenarioKind,
    pub iteration: u32,
    pub model_config_id: String,
    pub skill_revision_id: String,
    pub attempt: u32,
    pub status: String,
    #[serde(default = "default_runner_invocations")]
    pub runner_invocations: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    pub paths: RunPaths,
    pub mirror_path: String,
    pub artifacts: Vec<serde_json::Value>,
    pub metrics: RunMetrics,
    pub cache: Option<RunCacheInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_integrity: Option<SkillIntegrityReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn default_runner_invocations() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillIntegrityReport {
    pub tampered: bool,
    pub tampered_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunPaths {
    pub workspace: String,
    pub outputs: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunMetrics {
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SummariesSection {
    pub by_scenario: Vec<ScenarioSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_feedback: Option<HumanFeedbackSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioSummary {
    pub scenario_id: ScenarioKind,
    pub total_runs: usize,
    pub passed_runs: usize,
    pub skipped_runs: usize,
    pub failed_runs: usize,
}

pub fn build_report_bundle(
    fs: &impl FileSystem,
    skill_path: &Path,
    user_skill_path: &Path,
    skill_name: &str,
    model_config_label: &str,
    scenarios: &[ScenarioKind],
    options: BuildReportOptions,
) -> Result<ReportBundle> {
    if scenarios.is_empty() {
        return Err(EvalError::Validation(
            ValidationError::for_field("scenarios", "at least one scenario is required").into(),
        ));
    }

    let mut seen = HashSet::new();
    for scenario in scenarios {
        if !seen.insert(*scenario) {
            return Err(EvalError::Validation(
                ValidationError::for_field("scenarios", format!("duplicate scenario '{}'", scenario.as_str())).into(),
            ));
        }
    }

    let skill_md_path = skill_path.join("SKILL.md");
    let skill_md = fs.read_to_string(&skill_md_path)?;
    let skill_hash = sha256_digest(&skill_md);

    let (old_skill_path_str, old_skill_hash) = match &options.old_skill_path {
        Some(old_path) => {
            let old_skill_md = fs.read_to_string(&old_path.join("SKILL.md"))?;
            let old_hash = sha256_digest(&old_skill_md);
            let user_path = options
                .user_old_skill_path
                .as_ref()
                .map(|path| path_to_string(path))
                .unwrap_or_else(|| path_to_string(old_path));
            (Some(user_path), Some(old_hash))
        }
        None => (None, None),
    };

    let evals_path = skill_path.join("evals").join("evals.json");
    let evals_content = fs.read_to_string(&evals_path)?;
    let evals_hash = sha256_digest(&evals_content);
    let suite: EvalSuite = parse_eval_suite(&evals_content)?;
    let eval_slugs = slugs_for_suite(&suite);
    let iteration = options.iteration.unwrap_or(1);
    let attempts = options.attempts.max(1);

    let user_skill_path_str = path_to_string(user_skill_path);
    let evals_path_str = format!("{user_skill_path_str}/evals/evals.json");

    let dimensions = build_dimensions(
        &suite,
        &skill_hash,
        old_skill_hash.as_deref(),
        model_config_label,
        scenarios,
        &eval_slugs,
    );
    let (runs, workspace_dirs) = build_runs(
        &suite,
        scenarios,
        model_config_label,
        iteration,
        attempts,
        &eval_slugs,
        options.skill_staging,
    );
    let summaries = build_summaries(scenarios, suite.evals.len(), attempts);

    let report_id = options.report_id.unwrap_or_else(generate_report_id);
    let generated_at = options
        .generated_at
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));

    let document = ReportDocument {
        schema_version: SCHEMA_VERSION.to_string(),
        report: ReportSection {
            id: report_id.clone(),
            generated_at,
            iteration,
            producer: ProducerSection {
                name: "trg".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            runner: options.runner.clone(),
            runner_binary: options.runner_binary.clone(),
            runner_version: options.runner_version.clone(),
            ci: build_ci_section(),
        },
        suite: SuiteSection {
            skill_name: skill_name.to_string(),
            skill_path: user_skill_path_str,
            skill_hash: skill_hash.clone(),
            evals_path: evals_path_str,
            evals_hash,
            old_skill_path: old_skill_path_str,
            old_skill_hash,
        },
        dimensions,
        runs,
        assertion_results: Vec::new(),
        summaries: SummariesSection {
            by_scenario: summaries.by_scenario,
            human_feedback: None,
        },
        improvement_feedback: Vec::new(),
        comparisons: Vec::new(),
        iteration_summary: None,
    };

    Ok(ReportBundle {
        report_id,
        skill_name: skill_name.to_string(),
        document,
        workspace_dirs,
    })
}

#[derive(Debug, Clone, Default)]
pub struct WriteReportOptions {
    pub force: bool,
    pub iteration: u32,
}

pub fn write_report_bundle(out_root: &Path, bundle: &ReportBundle, options: WriteReportOptions) -> Result<PathBuf> {
    let report_dir = out_root.join(&bundle.skill_name).join(&bundle.report_id);

    ensure_iteration_available(
        out_root,
        &bundle.skill_name,
        options.iteration,
        options.force,
        Some(&report_dir),
    )?;

    if report_dir.try_exists()? {
        if options.force {
            std::fs::remove_dir_all(&report_dir)?;
        } else {
            return Err(EvalError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "report directory already exists: {} (pass --force to overwrite)",
                    report_dir.display()
                ),
            )));
        }
    }

    std::fs::create_dir_all(&report_dir)?;

    for relative_workspace_dir in &bundle.workspace_dirs {
        std::fs::create_dir_all(report_dir.join(relative_workspace_dir))?;
        std::fs::create_dir_all(report_dir.join(relative_workspace_dir).join(OUTPUTS_DIR))?;
    }

    let eval_slugs: std::collections::HashMap<_, _> = bundle
        .document
        .dimensions
        .eval_cases
        .iter()
        .map(|eval_case| (eval_case.id.clone(), eval_case.slug.clone()))
        .collect();
    write_docs_mirror_layout(&report_dir, bundle, options.iteration, &eval_slugs)?;

    let report_json = serde_json::to_string_pretty(&bundle.document)?;
    std::fs::write(report_dir.join("report.json"), report_json)?;

    Ok(report_dir)
}

pub fn sync_human_feedback(report_dir: &Path) -> Result<()> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path)?;
    let mut document: ReportDocument = serde_json::from_str(&content)?;

    let entries = load_run_feedback_entries(report_dir)?;
    let total_runs = document.runs.len();
    document.summaries.human_feedback = if entries.is_empty() {
        None
    } else {
        Some(summarize_feedback(total_runs, &entries))
    };
    document.improvement_feedback = collect_improvement_feedback(&entries);

    for run in document.runs.iter_mut() {
        run.artifacts.retain(|artifact| {
            artifact
                .get("kind")
                .and_then(|value| value.as_str())
                .is_none_or(|kind| kind != "human_feedback")
        });

        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        if !feedback_path.is_file() {
            continue;
        }

        let relative = feedback_path
            .strip_prefix(report_dir)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| feedback_path.display().to_string());

        run.artifacts.push(serde_json::json!({
            "kind": "human_feedback",
            "path": relative,
        }));
    }

    let report_json = serde_json::to_string_pretty(&document)?;
    std::fs::write(report_path, report_json)?;
    Ok(())
}

pub fn read_feedback_for_run(report_dir: &Path, workspace_rel: &str) -> Result<Option<FeedbackDocument>> {
    let feedback_path = feedback_path_for_run(report_dir, workspace_rel);
    if !feedback_path.is_file() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&feedback_path)?;
    Ok(Some(super::feedback::parse_feedback_document(&content)?))
}

fn build_dimensions(
    suite: &EvalSuite,
    skill_hash: &str,
    old_skill_hash: Option<&str>,
    model_config_label: &str,
    scenarios: &[ScenarioKind],
    eval_slugs: &std::collections::HashMap<String, String>,
) -> DimensionsSection {
    let mut assertions = Vec::new();
    let mut eval_cases = Vec::with_capacity(suite.evals.len());

    for eval_case in &suite.evals {
        let mut assertion_ids = Vec::with_capacity(eval_case.assertions.len());
        for (index, assertion) in eval_case.assertions.iter().enumerate() {
            let assertion_id = format!("{}:a{index}", eval_case.id);
            assertion_ids.push(assertion_id.clone());
            assertions.push(AssertionDimension {
                id: assertion_id,
                eval_case_id: eval_case.id.to_string(),
                text: assertion.as_str().to_string(),
            });
        }

        eval_cases.push(EvalCaseDimension {
            id: eval_case.id.to_string(),
            slug: eval_slugs
                .get(eval_case.id.as_str())
                .cloned()
                .unwrap_or_else(|| super::layout::eval_slug(eval_case.id.as_str())),
            prompt: eval_case.prompt.as_str().to_string(),
            expected_output: eval_case.expected_output.as_str().to_string(),
            files: eval_case.files.iter().map(|file| file.as_str().to_string()).collect(),
            assertion_ids,
        });
    }

    let mut skill_revisions = vec![SkillRevisionDimension {
        id: "current".to_string(),
        skill_hash: skill_hash.to_string(),
    }];
    if let Some(old_hash) = old_skill_hash {
        skill_revisions.push(SkillRevisionDimension {
            id: "old".to_string(),
            skill_hash: old_hash.to_string(),
        });
    }

    DimensionsSection {
        eval_cases,
        assertions,
        skill_revisions,
        model_configs: vec![ModelConfigDimension {
            id: model_config_label.to_string(),
            capture_status: "partial".to_string(),
            label: model_config_label.to_string(),
            parameters: serde_json::Map::new(),
            parameter_sources: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        }],
        scenarios: scenarios
            .iter()
            .map(|scenario| ScenarioDimension {
                id: *scenario,
                kind: *scenario,
            })
            .collect(),
        graders: Vec::new(),
    }
}

fn build_runs(
    suite: &EvalSuite,
    scenarios: &[ScenarioKind],
    model_config_label: &str,
    iteration: u32,
    attempts: u32,
    eval_slugs: &std::collections::HashMap<String, String>,
    _skill_staging: SkillStaging,
) -> (Vec<RunRecord>, Vec<String>) {
    let mut runs = Vec::new();
    let mut workspace_dirs = Vec::new();
    let mut run_number = 1usize;

    for eval_case in &suite.evals {
        let eval_slug = eval_slugs
            .get(eval_case.id.as_str())
            .cloned()
            .unwrap_or_else(|| super::layout::eval_slug(eval_case.id.as_str()));

        for scenario in scenarios {
            for attempt in 1..=attempts {
                let run_id = format!("run-{run_number:03}");
                let workspace_path = format!("runs/{run_id}/workspace");
                let outputs_path = format!("{workspace_path}/{OUTPUTS_DIR}");
                let mirror_path =
                    super::layout::scenario_mirror_path(iteration, &eval_slug, scenario.as_str(), attempt, attempts);
                workspace_dirs.push(workspace_path.clone());
                runs.push(RunRecord {
                    id: run_id,
                    eval_case_id: eval_case.id.to_string(),
                    eval_slug: eval_slug.clone(),
                    scenario_id: *scenario,
                    iteration,
                    model_config_id: model_config_label.to_string(),
                    skill_revision_id: match scenario {
                        ScenarioKind::OldSkill => "old".to_string(),
                        _ => "current".to_string(),
                    },
                    attempt,
                    status: "skipped".to_string(),
                    runner_invocations: 1,
                    failure_kind: None,
                    paths: RunPaths {
                        workspace: workspace_path,
                        outputs: outputs_path,
                    },
                    mirror_path,
                    artifacts: Vec::new(),
                    metrics: RunMetrics::default(),
                    cache: None,
                    skill_integrity: None,
                    warnings: Vec::new(),
                });
                run_number += 1;
            }
        }
    }

    (runs, workspace_dirs)
}

fn build_summaries(scenarios: &[ScenarioKind], eval_count: usize, attempts: u32) -> SummariesSection {
    let total_per_scenario = eval_count * attempts.max(1) as usize;
    SummariesSection {
        by_scenario: scenarios
            .iter()
            .map(|scenario| ScenarioSummary {
                scenario_id: *scenario,
                total_runs: total_per_scenario,
                passed_runs: 0,
                skipped_runs: eval_count,
                failed_runs: 0,
            })
            .collect(),
        human_feedback: None,
    }
}

fn build_ci_section() -> Option<CiSection> {
    if std::env::var("GITHUB_ACTIONS").ok().as_deref() != Some("true") {
        return None;
    }

    Some(CiSection {
        provider: "github-actions".to_string(),
        run_id: env_var("GITHUB_RUN_ID"),
        run_attempt: env_var("GITHUB_RUN_ATTEMPT"),
        workflow: env_var("GITHUB_WORKFLOW"),
        job: env_var("GITHUB_JOB"),
        commit: env_var("GITHUB_SHA"),
        git_ref: env_var("GITHUB_REF"),
    })
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn sha256_digest(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn generate_report_id() -> String {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{timestamp}-{}", random_hex_8())
}

fn random_hex_8() -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mixed = (seed as u64) ^ ((seed >> 64) as u64) ^ (std::process::id() as u64);
    format!("{:08x}", mixed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::MemFS;
    use std::path::Path;

    fn sample_suite() -> EvalSuite {
        serde_json::from_str(
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "files": ["fixture.txt"],
                        "assertions": ["assert a"]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "files": [],
                        "assertions": ["assert b1", "assert b2"]
                    }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn build_dimensions_from_suite() {
        let suite = sample_suite();
        let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
        let dimensions = build_dimensions(
            &suite,
            "sha256:abc",
            None,
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            &slugs,
        );

        assert_eq!(dimensions.eval_cases.len(), 2);
        assert_eq!(dimensions.eval_cases[0].slug, "case-a");
        assert_eq!(dimensions.eval_cases[0].assertion_ids, vec!["case-a:a0"]);
        assert_eq!(dimensions.eval_cases[1].assertion_ids, vec!["case-b:a0", "case-b:a1"]);
        assert_eq!(dimensions.assertions.len(), 3);
        assert_eq!(dimensions.assertions[0].id, "case-a:a0");
        assert_eq!(dimensions.assertions[0].eval_case_id, "case-a");
        assert_eq!(dimensions.scenarios.len(), 2);
        assert_eq!(dimensions.model_configs[0].capture_status, "partial");
        assert_eq!(dimensions.model_configs[0].label, "ci-default");
    }

    #[test]
    fn build_runs_multiple_attempts_produces_n_runs_per_eval_scenario() {
        let suite = sample_suite();
        let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
        let scenarios = [ScenarioKind::WithSkill];
        let (runs, workspace_dirs) = build_runs(&suite, &scenarios, "ci-default", 1, 3, &slugs, SkillStaging::Symlink);

        assert_eq!(runs.len(), 6);
        assert_eq!(workspace_dirs.len(), 6);
        for eval_case in ["case-a", "case-b"] {
            let case_runs: Vec<_> = runs.iter().filter(|r| r.eval_case_id == eval_case).collect();
            assert_eq!(case_runs.len(), 3);
            assert_eq!(case_runs[0].attempt, 1);
            assert_eq!(case_runs[1].attempt, 2);
            assert_eq!(case_runs[2].attempt, 3);
            assert_eq!(
                case_runs[0].mirror_path,
                format!("iteration-1/eval-{eval_case}/with_skill/attempt-1/")
            );
            assert_eq!(
                case_runs[2].mirror_path,
                format!("iteration-1/eval-{eval_case}/with_skill/attempt-3/")
            );
        }
    }

    #[test]
    fn build_runs_single_attempt_keeps_legacy_mirror_path() {
        let suite = sample_suite();
        let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
        let (runs, _) = build_runs(
            &suite,
            &[ScenarioKind::WithSkill],
            "ci-default",
            1,
            1,
            &slugs,
            SkillStaging::Symlink,
        );
        assert_eq!(runs[0].mirror_path, "iteration-1/eval-case-a/with_skill/");
        assert_eq!(runs[0].attempt, 1);
    }

    #[test]
    fn build_runs_orders_by_eval_case_then_scenario() {
        let suite = sample_suite();
        let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
        let scenarios = [ScenarioKind::WithSkill, ScenarioKind::WithoutSkill];
        let (runs, workspace_dirs) = build_runs(&suite, &scenarios, "ci-default", 2, 1, &slugs, SkillStaging::Symlink);

        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].id, "run-001");
        assert_eq!(runs[0].eval_case_id, "case-a");
        assert_eq!(runs[0].eval_slug, "case-a");
        assert_eq!(runs[0].iteration, 2);
        assert_eq!(runs[0].mirror_path, "iteration-2/eval-case-a/with_skill/");
        assert_eq!(runs[0].scenario_id, ScenarioKind::WithSkill);
        assert_eq!(runs[1].id, "run-002");
        assert_eq!(runs[1].eval_case_id, "case-a");
        assert_eq!(runs[1].scenario_id, ScenarioKind::WithoutSkill);
        assert_eq!(runs[2].id, "run-003");
        assert_eq!(runs[2].eval_case_id, "case-b");
        assert_eq!(runs[2].scenario_id, ScenarioKind::WithSkill);
        assert_eq!(runs[3].id, "run-004");
        assert_eq!(runs[3].eval_case_id, "case-b");
        assert_eq!(runs[3].scenario_id, ScenarioKind::WithoutSkill);
        assert_eq!(
            workspace_dirs,
            vec![
                "runs/run-001/workspace",
                "runs/run-002/workspace",
                "runs/run-003/workspace",
                "runs/run-004/workspace",
            ]
        );
        assert!(runs.iter().all(|run| run.status == "skipped"));
    }

    #[test]
    fn build_summaries_counts_skipped_runs_per_scenario() {
        let summaries = build_summaries(&[ScenarioKind::WithSkill, ScenarioKind::OldSkill], 2, 1);

        assert_eq!(summaries.by_scenario.len(), 2);
        assert_eq!(summaries.by_scenario[0].scenario_id, ScenarioKind::WithSkill);
        assert_eq!(summaries.by_scenario[0].total_runs, 2);
        assert_eq!(summaries.by_scenario[0].skipped_runs, 2);
        assert_eq!(summaries.by_scenario[0].passed_runs, 0);
        assert_eq!(summaries.by_scenario[1].scenario_id, ScenarioKind::OldSkill);
        assert_eq!(summaries.by_scenario[1].failed_runs, 0);
    }

    #[test]
    fn build_dimensions_includes_old_skill_revision_when_present() {
        let suite = sample_suite();
        let eval_slugs = slugs_for_suite(&suite);
        let dimensions = build_dimensions(
            &suite,
            "sha256:current",
            Some("sha256:old"),
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::OldSkill],
            &eval_slugs,
        );

        assert_eq!(dimensions.skill_revisions.len(), 2);
        assert_eq!(dimensions.skill_revisions[0].id, "current");
        assert_eq!(dimensions.skill_revisions[0].skill_hash, "sha256:current");
        assert_eq!(dimensions.skill_revisions[1].id, "old");
        assert_eq!(dimensions.skill_revisions[1].skill_hash, "sha256:old");
    }

    #[test]
    fn build_runs_assigns_old_revision_for_old_skill_scenario() {
        let suite = sample_suite();
        let eval_slugs = slugs_for_suite(&suite);
        let (runs, _) = build_runs(
            &suite,
            &[ScenarioKind::OldSkill],
            "ci-default",
            1,
            1,
            &eval_slugs,
            SkillStaging::Symlink,
        );

        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.skill_revision_id == "old"));
    }

    #[test]
    fn build_report_bundle_records_old_skill_metadata() {
        let fs = MemFS::new();
        let skill_path = Path::new("demo-skill");
        let old_skill_path = Path::new("demo-skill-old");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(
            old_skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: old\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "1",
                        "prompt": "p",
                        "expected_output": "o",
                        "assertions": ["a"]
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
            &[ScenarioKind::WithSkill, ScenarioKind::OldSkill],
            BuildReportOptions {
                report_id: Some("test-report-id".to_string()),
                generated_at: Some("2026-05-25T22:00:00Z".to_string()),
                iteration: Some(1),
                old_skill_path: Some(old_skill_path.to_path_buf()),
                user_old_skill_path: Some(PathBuf::from("path/to/old-skill")),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            bundle.document.suite.old_skill_path.as_deref(),
            Some("path/to/old-skill")
        );
        assert!(bundle
            .document
            .suite
            .old_skill_hash
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(bundle.document.dimensions.skill_revisions.len(), 2);
        assert_eq!(bundle.document.runs[1].skill_revision_id, "old");
    }

    #[test]
    fn build_report_bundle_hashes_and_paths() {
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
                        "id": "1",
                        "prompt": "p",
                        "expected_output": "o",
                        "assertions": ["a"]
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
                report_id: Some("test-report-id".to_string()),
                generated_at: Some("2026-05-25T22:00:00Z".to_string()),
                iteration: Some(1),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(bundle.report_id, "test-report-id");
        assert_eq!(bundle.document.schema_version, SCHEMA_VERSION);
        assert_eq!(bundle.document.report.iteration, 1);
        assert!(bundle.document.suite.skill_hash.starts_with("sha256:"));
        assert!(bundle.document.suite.evals_hash.starts_with("sha256:"));
        assert_eq!(bundle.document.suite.skill_path, "demo-skill");
        assert_eq!(bundle.document.runs.len(), 1);
        assert_eq!(bundle.workspace_dirs, vec!["runs/run-001/workspace"]);
    }

    #[test]
    fn write_report_bundle_creates_report_and_empty_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = ReportBundle {
            report_id: "report-123".to_string(),
            skill_name: "demo-skill".to_string(),
            document: ReportDocument {
                schema_version: SCHEMA_VERSION.to_string(),
                report: ReportSection {
                    id: "report-123".to_string(),
                    generated_at: "2026-05-25T22:00:00Z".to_string(),
                    iteration: 1,
                    producer: ProducerSection {
                        name: "trg".to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                    runner: None,
                    runner_binary: None,
                    runner_version: None,
                    ci: None,
                },
                suite: SuiteSection {
                    skill_name: "demo-skill".to_string(),
                    skill_path: "demo-skill".to_string(),
                    skill_hash: "sha256:deadbeef".to_string(),
                    evals_path: "demo-skill/evals/evals.json".to_string(),
                    evals_hash: "sha256:feedface".to_string(),
                    old_skill_path: None,
                    old_skill_hash: None,
                },
                dimensions: DimensionsSection {
                    eval_cases: Vec::new(),
                    assertions: Vec::new(),
                    skill_revisions: Vec::new(),
                    model_configs: Vec::new(),
                    scenarios: Vec::new(),
                    graders: Vec::new(),
                },
                runs: Vec::new(),
                assertion_results: Vec::new(),
                summaries: SummariesSection {
                    by_scenario: Vec::new(),
                    human_feedback: None,
                },
                improvement_feedback: Vec::new(),
                comparisons: Vec::new(),
                iteration_summary: None,
            },
            workspace_dirs: vec!["runs/run-001/workspace".to_string()],
        };

        let report_dir = write_report_bundle(
            temp.path(),
            &bundle,
            WriteReportOptions {
                force: false,
                iteration: 1,
            },
        )
        .unwrap();
        assert!(report_dir.join("report.json").is_file());
        assert!(report_dir.join("iteration-1/benchmark.json").is_file());
        let workspace_dir = report_dir.join("runs/run-001/workspace");
        assert!(workspace_dir.is_dir());
        assert!(workspace_dir.join(OUTPUTS_DIR).is_dir());
        assert_eq!(std::fs::read_dir(&workspace_dir).unwrap().count(), 1);

        let parsed: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            parsed.get("schema_version").and_then(|v| v.as_str()),
            Some(SCHEMA_VERSION)
        );
    }

    mod backward_compat {
        use super::*;
        use crate::agentskills::schemas::{validate_artifact, REPORT_SCHEMA};

        const FIXTURE_V1: &str = include_str!("testdata/reports/v1.json");
        const FIXTURE_V1_MINIMAL: &str = include_str!("testdata/reports/v1-minimal.json");

        fn load_fixture_json(name: &str, content: &str) -> serde_json::Value {
            serde_json::from_str(content).unwrap_or_else(|error| panic!("{name} is valid JSON: {error}"))
        }

        fn is_omitted_on_reserialize(original: &serde_json::Value) -> bool {
            match original {
                serde_json::Value::Null => true,
                serde_json::Value::Array(items) => items.is_empty(),
                serde_json::Value::Object(fields) => fields.is_empty(),
                _ => false,
            }
        }

        fn assert_round_trip_preserves_fields(original: &serde_json::Value, roundtrip: &serde_json::Value, path: &str) {
            if is_omitted_on_reserialize(original) {
                return;
            }

            match (original, roundtrip) {
                (serde_json::Value::Object(orig), serde_json::Value::Object(rt)) => {
                    for (key, orig_val) in orig {
                        if is_omitted_on_reserialize(orig_val) {
                            continue;
                        }
                        let child_path = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path}.{key}")
                        };
                        let rt_val = rt.get(key).unwrap_or_else(|| {
                            panic!(
                                "field {child_path} missing after round-trip (possible breaking change without schema_version bump)"
                            )
                        });
                        assert_round_trip_preserves_fields(orig_val, rt_val, &child_path);
                    }
                }
                (serde_json::Value::Array(orig_items), serde_json::Value::Array(rt_items)) => {
                    assert_eq!(orig_items.len(), rt_items.len(), "array length mismatch at {path}");
                    for (index, orig_item) in orig_items.iter().enumerate() {
                        assert_round_trip_preserves_fields(orig_item, &rt_items[index], &format!("{path}[{index}]"));
                    }
                }
                (orig, rt) => {
                    assert_eq!(orig, rt, "value mismatch at {path}");
                }
            }
        }

        #[test]
        fn report_v1_fixture_deserializes_and_round_trips() {
            let original = load_fixture_json("v1.json", FIXTURE_V1);
            let document: ReportDocument =
                serde_json::from_value(original.clone()).expect("v1 fixture deserializes into ReportDocument");
            assert_eq!(document.schema_version, SCHEMA_VERSION);

            let roundtrip = serde_json::to_value(&document).expect("ReportDocument serializes");
            assert_round_trip_preserves_fields(&original, &roundtrip, "");
        }

        #[test]
        fn report_v1_minimal_fixture_deserializes() {
            let original = load_fixture_json("v1-minimal.json", FIXTURE_V1_MINIMAL);
            let document: ReportDocument = serde_json::from_value(original)
                .expect("v1-minimal fixture deserializes (required fields must stay optional or defaulted)");
            assert_eq!(document.runs.len(), 0);
            assert_eq!(document.report.iteration, 1);
        }

        #[test]
        fn report_fixtures_validate_against_schema() {
            for (name, content) in [("v1.json", FIXTURE_V1), ("v1-minimal.json", FIXTURE_V1_MINIMAL)] {
                let value = load_fixture_json(name, content);
                validate_artifact(REPORT_SCHEMA, &value)
                    .unwrap_or_else(|error| panic!("{name} validates against REPORT_SCHEMA: {error}"));
            }
        }
    }
}
