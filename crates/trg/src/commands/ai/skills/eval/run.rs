use std::path::PathBuf;

use crate::agentskills::evals::EvalCheckOptions;
use crate::agentskills::report::{build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind};
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
        value_name = "KIND",
        default_values = ["with_skill"],
        help = "Scenario kind to include (repeatable)"
    )]
    pub scenario: Vec<String>,
}

impl RunArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let scenarios = match parse_scenarios(&self.scenario) {
            Ok(scenarios) => scenarios,
            Err(message) => {
                eprintln!("{message}");
                return 1;
            }
        };

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
            &scenarios,
            BuildReportOptions::default(),
        ) {
            Ok(bundle) => bundle,
            Err(e) => {
                eprintln!("Failed to build eval report bundle: {}", e);
                return 1;
            }
        };

        match write_report_bundle(&self.out, &bundle) {
            Ok(report_dir) => {
                println!("{}", report_dir.display());
                0
            }
            Err(e) => {
                eprintln!("Failed to write eval report bundle: {}", e);
                1
            }
        }
    }
}

fn parse_scenarios(values: &[String]) -> Result<Vec<ScenarioKind>, String> {
    let mut scenarios = Vec::with_capacity(values.len());
    for value in values {
        scenarios.push(ScenarioKind::parse(value)?);
    }
    Ok(scenarios)
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
            scenario: vec![
                ScenarioKind::WithSkill.as_str().to_string(),
                ScenarioKind::WithoutSkill.as_str().to_string(),
            ],
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

        let outputs_dir = report_dir.join("runs/run-001/outputs");
        assert!(outputs_dir.is_dir());
        assert_eq!(std::fs::read_dir(outputs_dir).unwrap().count(), 0);
    }

    #[test]
    fn parse_scenarios_rejects_unknown_kind() {
        let error = parse_scenarios(&["with_skill".to_string(), "bogus".to_string()]).unwrap_err();
        assert!(error.contains("bogus"));
    }
}
