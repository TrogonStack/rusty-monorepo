use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::fs::FileSystem;

use super::evals::{EvalError, EvalSuite, Result};
use super::validation::ValidationError;

pub const SCHEMA_VERSION: &str = "trg.skills-eval.report.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScenarioKind {
    WithSkill,
    WithoutSkill,
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

    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value {
            "with_skill" => Ok(Self::WithSkill),
            "without_skill" => Ok(Self::WithoutSkill),
            "old_skill" => Ok(Self::OldSkill),
            _ => Err(format!(
                "invalid scenario '{value}', expected with_skill, without_skill, or old_skill"
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BuildReportOptions {
    pub report_id: Option<String>,
    pub generated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReportBundle {
    pub report_id: String,
    pub skill_name: String,
    pub document: ReportDocument,
    pub output_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportDocument {
    pub schema_version: String,
    pub report: ReportSection,
    pub suite: SuiteSection,
    pub dimensions: DimensionsSection,
    pub runs: Vec<RunRecord>,
    pub assertion_results: Vec<serde_json::Value>,
    pub summaries: SummariesSection,
    pub comparisons: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportSection {
    pub id: String,
    pub generated_at: String,
    pub producer: ProducerSection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiSection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProducerSection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
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
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteSection {
    pub skill_name: String,
    pub skill_path: String,
    pub skill_hash: String,
    pub evals_path: String,
    pub evals_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DimensionsSection {
    pub eval_cases: Vec<EvalCaseDimension>,
    pub assertions: Vec<AssertionDimension>,
    pub skill_revisions: Vec<SkillRevisionDimension>,
    pub model_configs: Vec<ModelConfigDimension>,
    pub scenarios: Vec<ScenarioDimension>,
    pub graders: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCaseDimension {
    pub id: String,
    pub prompt: String,
    pub expected_output: String,
    pub files: Vec<String>,
    pub assertion_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssertionDimension {
    pub id: String,
    pub eval_case_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRevisionDimension {
    pub id: String,
    pub skill_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelConfigDimension {
    pub id: String,
    pub capture_status: String,
    pub label: String,
    pub parameters: serde_json::Map<String, serde_json::Value>,
    pub parameter_sources: serde_json::Map<String, serde_json::Value>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioDimension {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub id: String,
    pub eval_case_id: String,
    pub scenario_id: String,
    pub model_config_id: String,
    pub skill_revision_id: String,
    pub attempt: u32,
    pub status: String,
    pub paths: RunPaths,
    pub artifacts: Vec<serde_json::Value>,
    pub metrics: RunMetrics,
    pub cache: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunPaths {
    pub outputs: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunMetrics {
    pub duration_ms: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummariesSection {
    pub by_scenario: Vec<ScenarioSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioSummary {
    pub scenario_id: String,
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

    let skill_md_path = skill_path.join("SKILL.md");
    let skill_md = fs.read_to_string(&skill_md_path)?;
    let skill_hash = sha256_digest(&skill_md);

    let evals_path = skill_path.join("evals").join("evals.json");
    let evals_content = fs.read_to_string(&evals_path)?;
    let evals_hash = sha256_digest(&evals_content);
    let suite: EvalSuite = serde_json::from_str(&evals_content)?;

    let user_skill_path_str = path_to_string(user_skill_path);
    let evals_path_str = format!("{user_skill_path_str}/evals/evals.json");

    let dimensions = build_dimensions(&suite, &skill_hash, model_config_label, scenarios);
    let (runs, output_dirs) = build_runs(&suite, scenarios, model_config_label);
    let summaries = build_summaries(scenarios, suite.evals.len());

    let report_id = options.report_id.unwrap_or_else(generate_report_id);
    let generated_at = options
        .generated_at
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));

    let document = ReportDocument {
        schema_version: SCHEMA_VERSION.to_string(),
        report: ReportSection {
            id: report_id.clone(),
            generated_at,
            producer: ProducerSection {
                name: "trg".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            ci: build_ci_section(),
        },
        suite: SuiteSection {
            skill_name: skill_name.to_string(),
            skill_path: user_skill_path_str,
            skill_hash: skill_hash.clone(),
            evals_path: evals_path_str,
            evals_hash,
        },
        dimensions,
        runs,
        assertion_results: Vec::new(),
        summaries,
        comparisons: Vec::new(),
    };

    Ok(ReportBundle {
        report_id,
        skill_name: skill_name.to_string(),
        document,
        output_dirs,
    })
}

pub fn write_report_bundle(out_root: &Path, bundle: &ReportBundle) -> Result<PathBuf> {
    let report_dir = out_root.join(&bundle.skill_name).join(&bundle.report_id);

    std::fs::create_dir_all(&report_dir)?;

    for relative_output_dir in &bundle.output_dirs {
        std::fs::create_dir_all(report_dir.join(relative_output_dir))?;
    }

    let report_json = serde_json::to_string_pretty(&bundle.document)?;
    std::fs::write(report_dir.join("report.json"), report_json)?;

    Ok(report_dir)
}

fn build_dimensions(
    suite: &EvalSuite,
    skill_hash: &str,
    model_config_label: &str,
    scenarios: &[ScenarioKind],
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
                text: assertion.clone(),
            });
        }

        eval_cases.push(EvalCaseDimension {
            id: eval_case.id.to_string(),
            prompt: eval_case.prompt.clone(),
            expected_output: eval_case.expected_output.clone(),
            files: eval_case.files.clone(),
            assertion_ids,
        });
    }

    DimensionsSection {
        eval_cases,
        assertions,
        skill_revisions: vec![SkillRevisionDimension {
            id: "current".to_string(),
            skill_hash: skill_hash.to_string(),
        }],
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
                id: scenario.as_str().to_string(),
                kind: scenario.as_str().to_string(),
            })
            .collect(),
        graders: Vec::new(),
    }
}

fn build_runs(
    suite: &EvalSuite,
    scenarios: &[ScenarioKind],
    model_config_label: &str,
) -> (Vec<RunRecord>, Vec<String>) {
    let mut runs = Vec::new();
    let mut output_dirs = Vec::new();
    let mut run_number = 1usize;

    for eval_case in &suite.evals {
        for scenario in scenarios {
            let run_id = format!("run-{run_number:03}");
            let outputs_path = format!("runs/{run_id}/outputs");
            output_dirs.push(outputs_path.clone());
            runs.push(RunRecord {
                id: run_id,
                eval_case_id: eval_case.id.to_string(),
                scenario_id: scenario.as_str().to_string(),
                model_config_id: model_config_label.to_string(),
                skill_revision_id: "current".to_string(),
                attempt: 1,
                status: "skipped".to_string(),
                paths: RunPaths { outputs: outputs_path },
                artifacts: Vec::new(),
                metrics: RunMetrics {
                    duration_ms: None,
                    total_tokens: None,
                    input_tokens: None,
                    output_tokens: None,
                    cost_usd: None,
                },
                cache: None,
            });
            run_number += 1;
        }
    }

    (runs, output_dirs)
}

fn build_summaries(scenarios: &[ScenarioKind], eval_count: usize) -> SummariesSection {
    SummariesSection {
        by_scenario: scenarios
            .iter()
            .map(|scenario| ScenarioSummary {
                scenario_id: scenario.as_str().to_string(),
                total_runs: eval_count,
                passed_runs: 0,
                skipped_runs: eval_count,
                failed_runs: 0,
            })
            .collect(),
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
    format!("{mixed:08x}")
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
        let dimensions = build_dimensions(
            &suite,
            "sha256:abc",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        );

        assert_eq!(dimensions.eval_cases.len(), 2);
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
    fn build_runs_orders_by_eval_case_then_scenario() {
        let suite = sample_suite();
        let scenarios = [ScenarioKind::WithSkill, ScenarioKind::WithoutSkill];
        let (runs, output_dirs) = build_runs(&suite, &scenarios, "ci-default");

        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].id, "run-001");
        assert_eq!(runs[0].eval_case_id, "case-a");
        assert_eq!(runs[0].scenario_id, "with_skill");
        assert_eq!(runs[1].id, "run-002");
        assert_eq!(runs[1].eval_case_id, "case-a");
        assert_eq!(runs[1].scenario_id, "without_skill");
        assert_eq!(runs[2].id, "run-003");
        assert_eq!(runs[2].eval_case_id, "case-b");
        assert_eq!(runs[2].scenario_id, "with_skill");
        assert_eq!(runs[3].id, "run-004");
        assert_eq!(runs[3].eval_case_id, "case-b");
        assert_eq!(runs[3].scenario_id, "without_skill");
        assert_eq!(
            output_dirs,
            vec![
                "runs/run-001/outputs",
                "runs/run-002/outputs",
                "runs/run-003/outputs",
                "runs/run-004/outputs",
            ]
        );
        assert!(runs.iter().all(|run| run.status == "skipped"));
    }

    #[test]
    fn build_summaries_counts_skipped_runs_per_scenario() {
        let summaries = build_summaries(&[ScenarioKind::WithSkill, ScenarioKind::OldSkill], 2);

        assert_eq!(summaries.by_scenario.len(), 2);
        assert_eq!(summaries.by_scenario[0].scenario_id, "with_skill");
        assert_eq!(summaries.by_scenario[0].total_runs, 2);
        assert_eq!(summaries.by_scenario[0].skipped_runs, 2);
        assert_eq!(summaries.by_scenario[0].passed_runs, 0);
        assert_eq!(summaries.by_scenario[1].scenario_id, "old_skill");
        assert_eq!(summaries.by_scenario[1].failed_runs, 0);
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
            },
        )
        .unwrap();

        assert_eq!(bundle.report_id, "test-report-id");
        assert_eq!(bundle.document.schema_version, SCHEMA_VERSION);
        assert!(bundle.document.suite.skill_hash.starts_with("sha256:"));
        assert!(bundle.document.suite.evals_hash.starts_with("sha256:"));
        assert_eq!(bundle.document.suite.skill_path, "demo-skill");
        assert_eq!(bundle.document.runs.len(), 1);
        assert_eq!(bundle.output_dirs, vec!["runs/run-001/outputs"]);
    }

    #[test]
    fn write_report_bundle_creates_report_and_empty_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = ReportBundle {
            report_id: "report-123".to_string(),
            skill_name: "demo-skill".to_string(),
            document: ReportDocument {
                schema_version: SCHEMA_VERSION.to_string(),
                report: ReportSection {
                    id: "report-123".to_string(),
                    generated_at: "2026-05-25T22:00:00Z".to_string(),
                    producer: ProducerSection {
                        name: "trg".to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                    ci: None,
                },
                suite: SuiteSection {
                    skill_name: "demo-skill".to_string(),
                    skill_path: "demo-skill".to_string(),
                    skill_hash: "sha256:deadbeef".to_string(),
                    evals_path: "demo-skill/evals/evals.json".to_string(),
                    evals_hash: "sha256:feedface".to_string(),
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
                },
                comparisons: Vec::new(),
            },
            output_dirs: vec!["runs/run-001/outputs".to_string()],
        };

        let report_dir = write_report_bundle(temp.path(), &bundle).unwrap();
        assert!(report_dir.join("report.json").is_file());
        let outputs_dir = report_dir.join("runs/run-001/outputs");
        assert!(outputs_dir.is_dir());
        assert_eq!(std::fs::read_dir(&outputs_dir).unwrap().count(), 0);

        let parsed: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            parsed.get("schema_version").and_then(|v| v.as_str()),
            Some(SCHEMA_VERSION)
        );
    }
}
