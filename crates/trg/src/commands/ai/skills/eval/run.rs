use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::agentskills::evals::{EvalCase, EvalCheckOptions, EvalSuite};
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ReportBundle, ScenarioKind, SkillIntegrityReport,
};
use crate::agentskills::runner::{compute_skill_digest, detect_tampering, EvalRunOutcome, EvalRunRequest, Runner};
use crate::fs::FileSystem;
use clap::Args;

use crate::commands::ai::skills::resolve_skill_path;

#[derive(Args)]
pub struct RunArgs {
    #[arg(help = "Path to skill directory or SKILL.md file")]
    pub path: PathBuf,

    #[arg(long, value_name = "DIR", help = "Root directory for the generated artifact bundle")]
    pub out: PathBuf,

    #[arg(
        long,
        value_name = "LABEL",
        default_value = "ci-default",
        help = "Opaque model configuration label recorded in report.json"
    )]
    pub model_config: String,

    #[arg(
        long,
        value_enum,
        value_name = "KIND",
        default_values_t = [ScenarioKind::WithSkill],
        help = "Scenario kind to include (repeatable)"
    )]
    pub scenario: Vec<ScenarioKind>,

    #[arg(
        long,
        value_enum,
        value_name = "RUNNER",
        help = "Agent CLI to execute each (eval × scenario). When unset, runs are scaffolded with status: skipped."
    )]
    pub runner: Option<Runner>,

    #[arg(
        long,
        value_name = "MODEL",
        help = "Optional model identifier forwarded to the runner CLI (--model/-m). When unset, the runner CLI picks its own default; CLI-specific string."
    )]
    pub runner_model: Option<String>,
}

impl RunArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let skill_path = resolve_skill_path(&self.path);
        let props = match crate::agentskills::validator::validate_skill(fs, &skill_path) {
            Ok(props) => props,
            Err(e) => {
                eprintln!("Skill validation failed: {}", e);
                return 1;
            }
        };

        if let Err(e) =
            crate::agentskills::evals::check_eval_suite(fs, &skill_path, &props.name, EvalCheckOptions::default())
        {
            eprintln!("Skill eval validation failed: {}", e);
            return 1;
        }

        let bundle = match build_report_bundle(
            fs,
            &skill_path,
            &self.path,
            &props.name,
            &self.model_config,
            &self.scenario,
            BuildReportOptions::default(),
        ) {
            Ok(bundle) => bundle,
            Err(e) => {
                eprintln!("Failed to build eval report bundle: {}", e);
                return 1;
            }
        };

        let report_dir = match write_report_bundle(&self.out, &bundle) {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("Failed to write eval report bundle: {}", e);
                return 1;
            }
        };

        if let Some(runner) = self.runner {
            if let Err(code) = execute_runs(runner, self.runner_model.as_deref(), &skill_path, &report_dir, bundle) {
                return code;
            }
        }

        println!("{}", report_dir.display());
        0
    }
}

fn execute_runs(
    runner: Runner,
    runner_model: Option<&str>,
    skill_path: &Path,
    report_dir: &Path,
    mut bundle: ReportBundle,
) -> std::result::Result<(), i32> {
    let skill_md = match std::fs::read_to_string(skill_path.join("SKILL.md")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read SKILL.md: {}", e);
            return Err(1);
        }
    };

    let evals_path = skill_path.join("evals").join("evals.json");
    let suite: EvalSuite = match std::fs::read_to_string(&evals_path)
        .map_err(|e| format!("read {}: {e}", evals_path.display()))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("parse {}: {e}", evals_path.display())))
    {
        Ok(suite) => suite,
        Err(msg) => {
            eprintln!("Failed to load eval suite: {}", msg);
            return Err(1);
        }
    };

    let case_index: HashMap<String, &EvalCase> = suite.evals.iter().map(|c| (c.id.to_string(), c)).collect();

    for run in bundle.document.runs.iter_mut() {
        let case = match case_index.get(&run.eval_case_id) {
            Some(case) => *case,
            None => {
                eprintln!("Skipping run {}: eval case {} not found", run.id, run.eval_case_id);
                continue;
            }
        };

        let scenario = match parse_scenario(&run.scenario_id) {
            Some(s) => s,
            None => {
                eprintln!("Skipping run {}: unknown scenario {}", run.id, run.scenario_id);
                continue;
            }
        };

        let workspace_dir = report_dir.join(&run.paths.workspace);
        let run_dir = workspace_dir.parent().unwrap_or(report_dir).to_path_buf();
        let transcript_path = run_dir.join("transcript.jsonl");

        let request = EvalRunRequest {
            eval: case,
            scenario,
            skill_md: &skill_md,
            skill_path,
            workspace_dir: &workspace_dir,
            transcript_path: &transcript_path,
            runner_model,
        };

        let digest_before = match compute_skill_digest(skill_path) {
            Ok(digest) => Some(digest),
            Err(e) => {
                eprintln!("Run {}: failed to hash skill before invoke: {}", run.id, e);
                None
            }
        };

        match runner.invoke(&request) {
            Ok(outcome) => apply_outcome(run, &outcome, &transcript_path, report_dir),
            Err(e) => {
                eprintln!("Run {} failed: {}", run.id, e);
                run.status = "failed".to_string();
            }
        }

        if let Some(before) = digest_before {
            match compute_skill_digest(skill_path) {
                Ok(after) => {
                    let tampered_files = detect_tampering(&before, &after);
                    run.skill_integrity = Some(SkillIntegrityReport {
                        tampered: !tampered_files.is_empty(),
                        tampered_files,
                    });
                }
                Err(e) => {
                    eprintln!("Run {}: failed to hash skill after invoke: {}", run.id, e);
                }
            }
        }
    }

    rebuild_summaries(&mut bundle);

    let report_json = match serde_json::to_string_pretty(&bundle.document) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to re-serialize report.json: {}", e);
            return Err(1);
        }
    };
    if let Err(e) = std::fs::write(report_dir.join("report.json"), report_json) {
        eprintln!("Failed to write updated report.json: {}", e);
        return Err(1);
    }

    Ok(())
}

fn parse_scenario(id: &str) -> Option<ScenarioKind> {
    match id {
        "with_skill" => Some(ScenarioKind::WithSkill),
        "without_skill" => Some(ScenarioKind::WithoutSkill),
        "old_skill" => Some(ScenarioKind::OldSkill),
        _ => None,
    }
}

fn apply_outcome(
    run: &mut crate::agentskills::report::RunRecord,
    outcome: &EvalRunOutcome,
    transcript_path: &Path,
    report_dir: &Path,
) {
    run.status = outcome.status.as_str().to_string();
    run.metrics.duration_ms = Some(outcome.duration_ms);
    run.metrics.total_tokens = outcome.total_tokens;
    run.metrics.input_tokens = outcome.input_tokens;
    run.metrics.output_tokens = outcome.output_tokens;
    run.metrics.cost_usd = outcome.cost_usd;

    let relative = transcript_path
        .strip_prefix(report_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| transcript_path.display().to_string());

    run.artifacts.push(serde_json::json!({
        "kind": "transcript",
        "path": relative,
    }));
}

fn rebuild_summaries(bundle: &mut ReportBundle) {
    let mut counts: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
    for run in &bundle.document.runs {
        let entry = counts.entry(run.scenario_id.clone()).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        match run.status.as_str() {
            "completed" => entry.1 += 1,
            "skipped" => entry.2 += 1,
            "failed" => entry.3 += 1,
            _ => {}
        }
    }
    for summary in bundle.document.summaries.by_scenario.iter_mut() {
        if let Some((total, passed, skipped, failed)) = counts.get(&summary.scenario_id) {
            summary.total_runs = *total;
            summary.passed_runs = *passed;
            summary.skipped_runs = *skipped;
            summary.failed_runs = *failed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::ScenarioKind;
    use std::path::Path;

    fn write_fixture_skill(root: &Path) -> PathBuf {
        let skill_dir = root.join("fixture-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "assertions": ["checks first"]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "assertions": ["checks second"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn run_command_builds_expected_bundle_layout() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_fixture_skill(temp.path());
        let out_dir = temp.path().join("artifacts");

        let status = RunArgs {
            path: skill_dir.clone(),
            out: out_dir.clone(),
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            runner: None,
            runner_model: None,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 0);

        let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(report_dirs.len(), 1);

        let report_dir = &report_dirs[0];
        let report_json_path = report_dir.join("report.json");
        assert!(report_json_path.is_file());

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_json_path).unwrap()).unwrap();
        assert_eq!(
            report.get("schema_version").and_then(|value| value.as_str()),
            Some(crate::agentskills::report::SCHEMA_VERSION)
        );
        assert!(report
            .pointer("/suite/skill_hash")
            .and_then(|value| value.as_str())
            .unwrap()
            .starts_with("sha256:"));
        assert!(report
            .pointer("/suite/evals_hash")
            .and_then(|value| value.as_str())
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(report.get("runs").and_then(|value| value.as_array()).unwrap().len(), 4);

        let workspace_dir = report_dir.join("runs/run-001/workspace");
        assert!(workspace_dir.is_dir());
        assert_eq!(std::fs::read_dir(workspace_dir).unwrap().count(), 0);
    }
}
