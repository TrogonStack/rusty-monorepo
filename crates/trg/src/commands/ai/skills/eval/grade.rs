use std::path::{Path, PathBuf};

use crate::agentskills::grading::{grade_report_bundle, GradeOptions, GraderMode};
use crate::fs::FileSystem;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval grade ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval grade ./report --grader auto --strict

  $ trg ai skills eval grade /absolute/path/to/report --grader script --grader-command ./grade.sh
")]
pub struct GradeArgs {
    #[arg(help = "Path to a report bundle directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(long, value_enum, default_value_t = GraderMode::Auto, help = "Grading strategy")]
    pub grader: GraderMode,

    #[arg(long, value_name = "MODEL", help = "Model identifier for LLM grading")]
    pub grader_model: Option<String>,

    #[arg(
        long,
        value_name = "COMMAND",
        help = "External grader script. Reads JSON from stdin: {assertion, workspace, outputs, transcript}. Writes {passed, evidence, rationale?} to stdout."
    )]
    pub grader_command: Option<String>,

    #[arg(long, help = "Fail when evidence is missing or assertions require LLM grading")]
    pub strict: bool,
}

impl GradeArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let options = GradeOptions {
            grader: self.grader,
            grader_model: self.grader_model,
            grader_command: self.grader_command,
            strict: self.strict,
        };
        grade_report_dir(&self.report_dir, options)
    }
}

pub(crate) fn grade_report_dir(report_dir: &Path, options: GradeOptions) -> i32 {
    grade_report_dir_with_report(report_dir, options).0
}

pub(crate) fn grade_report_dir_with_report(
    report_dir: &Path,
    options: GradeOptions,
) -> (i32, Option<crate::agentskills::grading::GradeReport>) {
    let strict = options.strict;
    match grade_report_bundle(report_dir, options) {
        Ok(report) => {
            print_report_dir(report_dir);
            println!("Graded {} run(s)", report.runs_graded);
            println!("  assertions: {}/{} passed", report.passed, report.assertions_graded);
            if report.needs_llm > 0 {
                println!("  needs LLM: {}", report.needs_llm);
            }
            let exit_code = if report.failed > 0 || (strict && report.needs_llm > 0) {
                1
            } else {
                0
            };
            (exit_code, Some(report))
        }
        Err(e) => {
            eprintln!("Grading failed: {}", e);
            (1, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_fixture_skill(root: &Path) -> std::path::PathBuf {
        let skill_dir = root.join("fixture-skill");
        fs::create_dir_all(skill_dir.join("evals")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: fixture\n---\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "assertions": ["file \"out.json\" exists"]
                    }
                ]
            }"#,
        )
        .unwrap();
        skill_dir
    }

    #[test]
    fn grade_command_writes_grading_json_for_report_bundle() {
        let temp = tempdir().unwrap();
        let skill_dir = write_fixture_skill(temp.path());

        let fs = MemFS::new();
        fs.insert(
            skill_dir.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: fixture\n---\n",
        );
        fs.insert(
            skill_dir.join("evals/evals.json"),
            fs::read_to_string(skill_dir.join("evals/evals.json")).unwrap(),
        );

        let bundle = build_report_bundle(
            &fs,
            &skill_dir,
            &skill_dir,
            "fixture-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("test-report".to_string()),
                generated_at: Some("2026-05-26T00:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
        let run_dir = report_dir.join("runs/run-001");
        fs::create_dir_all(run_dir.join("outputs")).unwrap();
        fs::write(run_dir.join("outputs/out.json"), r#"{"ok": true}"#).unwrap();

        let status = GradeArgs {
            report_dir: report_dir.clone(),
            grader: GraderMode::Auto,
            grader_model: None,
            grader_command: None,
            strict: false,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 0);
        let grading_path = run_dir.join("grading.json");
        assert!(grading_path.is_file());
        let grading: serde_json::Value = serde_json::from_str(&fs::read_to_string(&grading_path).unwrap()).unwrap();
        assert_eq!(
            grading.pointer("/assertion_results/0/passed").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn grade_command_integration_with_script_grader() {
        let temp = tempdir().unwrap();
        let skill_dir = write_fixture_skill(temp.path());

        let script = temp.path().join("grader.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
read _input
echo '{"passed": true, "evidence": "script confirmed custom check", "rationale": "integration"}'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let fs = MemFS::new();
        fs.insert(
            skill_dir.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: fixture\n---\n",
        );
        fs.insert(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "assertions": ["custom check passes"]
                    }
                ]
            }"#,
        );

        let bundle = build_report_bundle(
            &fs,
            &skill_dir,
            &skill_dir,
            "fixture-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("script-report".to_string()),
                generated_at: Some("2026-05-26T00:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
        fs::create_dir_all(report_dir.join("runs/run-001/workspace")).unwrap();

        let status = GradeArgs {
            report_dir,
            grader: GraderMode::Script,
            grader_model: None,
            grader_command: Some(script.to_string_lossy().into_owned()),
            strict: false,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, 0);
    }
}
