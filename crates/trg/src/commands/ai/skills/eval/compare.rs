use std::path::PathBuf;

use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::agentskills::compare::{run_comparisons, CompareOptions, ComparisonRecord, JudgeKind, ScenarioPair};
use crate::agentskills::eval_suite_drift::{
    detect_eval_suite_drift_snapshots, load_report_drift_snapshot, maybe_emit_eval_suite_drift_warning,
    EvalSuiteDriftWarning,
};
use crate::agentskills::iteration_summary::detect_previous_report_dir;
use crate::fs::FileSystem;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum CompareJudge {
    None,
    Llm,
    Script,
}

impl From<CompareJudge> for JudgeKind {
    fn from(value: CompareJudge) -> Self {
        match value {
            CompareJudge::None => Self::None,
            CompareJudge::Llm => Self::Llm,
            CompareJudge::Script => Self::Script,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CompareJsonOutput {
    pub report_dir: String,
    pub comparisons: Vec<ComparisonRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<EvalSuiteDriftWarning>,
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval compare ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval compare ./report --pair with_skill:without_skill --judge none

  $ trg ai skills eval compare ./report --pair with_skill:old_skill --emit-comparison-json

  $ trg ai skills eval compare ./report --previous ./artifacts/my-skill/prior-report --json
")]
pub struct CompareArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "DIR",
        help = "Previous iteration report directory for cross-iteration drift detection (auto-detected when omitted)"
    )]
    pub previous: Option<PathBuf>,

    #[arg(
        long,
        value_name = "A:B",
        help = "Scenario pair to compare (repeatable). Each side is with_skill, without_skill, or old_skill."
    )]
    pub pair: Vec<String>,

    #[arg(long, value_enum, default_value_t = CompareJudge::None)]
    pub judge: CompareJudge,

    #[arg(long, value_name = "MODEL", help = "Model identifier for LLM judging")]
    pub judge_model: Option<String>,

    #[arg(
        long,
        value_name = "COMMAND",
        help = "External judge command (reads JSON from stdin, writes JSON to stdout)"
    )]
    pub judge_command: Option<String>,

    #[arg(long, help = "Write comparison.json under iteration layout directories when present")]
    pub emit_comparison_json: bool,

    #[arg(
        long,
        help = "Suppress the warning when the eval suite hash differs from the previous iteration report"
    )]
    pub allow_eval_suite_drift: bool,

    #[arg(long, help = "Emit comparison results as JSON to stdout")]
    pub json: bool,
}

impl CompareArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let pairs = match self
            .pair
            .iter()
            .map(|raw| ScenarioPair::parse(raw))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(pairs) => pairs,
            Err(error) => {
                eprintln!("Invalid comparison pair: {error}");
                return 1;
            }
        };

        let warnings = match collect_eval_suite_drift_warnings(
            &self.report_dir,
            self.previous.as_deref(),
            self.allow_eval_suite_drift,
        ) {
            Ok(warnings) => warnings,
            Err(error) => {
                eprintln!("Failed to check eval suite drift: {error}");
                return 1;
            }
        };

        match run_comparisons(
            &self.report_dir,
            CompareOptions {
                pairs,
                judge: self.judge.into(),
                judge_model: self.judge_model,
                judge_command: self.judge_command,
                emit_comparison_json: self.emit_comparison_json,
            },
        ) {
            Ok(records) => {
                if self.json {
                    let output = CompareJsonOutput {
                        report_dir: self.report_dir.display().to_string(),
                        comparisons: records,
                        warnings,
                    };
                    match serde_json::to_string_pretty(&output) {
                        Ok(json) => println!("{json}"),
                        Err(error) => {
                            eprintln!("Failed to serialize compare output: {error}");
                            return 1;
                        }
                    }
                } else if records.is_empty() {
                    println!("Comparison skipped");
                } else {
                    println!("Compared {} eval/pair record(s)", records.len());
                }
                0
            }
            Err(error) => {
                eprintln!("Comparison failed: {error}");
                1
            }
        }
    }
}

fn collect_eval_suite_drift_warnings(
    report_dir: &std::path::Path,
    previous_override: Option<&std::path::Path>,
    allow_eval_suite_drift: bool,
) -> Result<Vec<EvalSuiteDriftWarning>, crate::agentskills::evals::EvalError> {
    let current = load_report_drift_snapshot(report_dir)?;
    let previous_dir = previous_override
        .map(PathBuf::from)
        .or_else(|| detect_previous_report_dir(report_dir, current.iteration));

    let Some(previous_dir) = previous_dir else {
        return Ok(Vec::new());
    };

    let previous = load_report_drift_snapshot(&previous_dir)?;
    let drift = detect_eval_suite_drift_snapshots(&current, &previous);
    maybe_emit_eval_suite_drift_warning(drift.as_ref(), allow_eval_suite_drift);

    Ok(drift
        .as_ref()
        .map(|report| vec![EvalSuiteDriftWarning::from(report)])
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_cmd::Command;
    use std::fs;

    fn write_minimal_report(dir: &std::path::Path, iteration: u32, evals_hash: &str, eval_ids: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        let eval_cases: Vec<serde_json::Value> = eval_ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "slug": id,
                    "prompt": "p",
                    "expected_output": "o",
                    "files": [],
                    "assertion_ids": []
                })
            })
            .collect();
        let report = serde_json::json!({
            "schema_version": "trg.skills-eval.report.v1",
            "report": {
                "id": format!("report-iter-{iteration}"),
                "generated_at": "2026-05-26T12:00:00Z",
                "iteration": iteration,
                "producer": { "name": "trg", "version": "0.3.0" }
            },
            "suite": {
                "skill_name": "demo-skill",
                "skill_path": "demo-skill",
                "skill_hash": "sha256:abc",
                "evals_path": "demo-skill/evals/evals.json",
                "evals_hash": evals_hash
            },
            "dimensions": {
                "eval_cases": eval_cases,
                "assertions": [],
                "skill_revisions": [],
                "model_configs": [],
                "scenarios": [],
                "graders": []
            },
            "runs": [],
            "assertion_results": [],
            "summaries": { "by_scenario": [] },
            "comparisons": []
        });
        fs::write(dir.join("report.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }

    #[test]
    fn compare_warns_on_eval_suite_drift() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");
        write_minimal_report(&previous, 1, "sha256:1111", &["case-a", "case-b"]);
        write_minimal_report(&current, 2, "sha256:2222", &["case-a", "case-c"]);

        let output = Command::cargo_bin("trg")
            .unwrap()
            .args([
                "ai",
                "skills",
                "eval",
                "compare",
                current.to_str().unwrap(),
                "--previous",
                previous.to_str().unwrap(),
            ])
            .output()
            .unwrap();

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("WARN: eval suite changed between iterations"));
        assert!(stderr.contains("added eval IDs: case-c"));
        assert!(stderr.contains("removed eval IDs: case-b"));
    }

    #[test]
    fn compare_allow_eval_suite_drift_suppresses_warning() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");
        write_minimal_report(&previous, 1, "sha256:1111", &["case-a"]);
        write_minimal_report(&current, 2, "sha256:2222", &["case-b"]);

        let output = Command::cargo_bin("trg")
            .unwrap()
            .args([
                "ai",
                "skills",
                "eval",
                "compare",
                current.to_str().unwrap(),
                "--previous",
                previous.to_str().unwrap(),
                "--allow-eval-suite-drift",
                "--json",
            ])
            .output()
            .unwrap();

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("WARN: eval suite changed between iterations"));

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let warning = &json["warnings"][0];
        assert_eq!(warning["kind"], "eval_suite_drift");
        assert_eq!(warning["added_eval_ids"], serde_json::json!(["case-b"]));
        assert_eq!(warning["removed_eval_ids"], serde_json::json!(["case-a"]));
    }

    #[test]
    fn compare_help_documents_allow_eval_suite_drift() {
        let help = CompareArgs::augment_args(clap::Command::new("compare").about("compare"))
            .render_long_help()
            .to_string();
        assert!(help.contains("--allow-eval-suite-drift"));
    }
}
