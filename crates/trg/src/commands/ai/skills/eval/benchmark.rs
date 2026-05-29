use std::path::{Path, PathBuf};

use crate::agentskills::benchmark::{build_benchmark, write_benchmark, BenchmarkOptions, FailedRunsMode};
use crate::fs::FileSystem;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval benchmark ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval benchmark ./report --failed-runs exclude

  $ trg ai skills eval benchmark ./report --previous ./artifacts/my-skill/prior-report --json

  $ trg ai skills eval benchmark /absolute/path/to/report
")]
pub struct BenchmarkArgs {
    #[arg(help = "Path to the report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "DIR",
        help = "Previous iteration report directory for cross-iteration drift detection (auto-detected when omitted)"
    )]
    pub previous: Option<PathBuf>,

    #[arg(
        long,
        value_enum,
        default_value_t = FailedRunsMode::Bucket,
        help = "How to treat runner failures when aggregating pass rates"
    )]
    pub failed_runs: FailedRunsMode,

    #[arg(
        long,
        help = "Suppress the warning when the eval suite hash differs from the previous iteration report"
    )]
    pub allow_eval_suite_drift: bool,

    #[arg(long, help = "Emit benchmark.json contents as JSON to stdout")]
    pub json: bool,
}

impl BenchmarkArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let options = BenchmarkOptions {
            failed_runs: self.failed_runs,
            allow_eval_suite_drift: self.allow_eval_suite_drift,
            previous_report_dir: self.previous,
        };

        let (code, document) = benchmark_report_dir_with_document(&self.report_dir, options);
        if code != 0 {
            return code;
        }

        if self.json {
            if let Some(document) = document {
                match serde_json::to_string_pretty(&document) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("Failed to serialize benchmark: {error}");
                        return 1;
                    }
                }
            }
        } else {
            print_report_dir(&self.report_dir);
        }
        0
    }
}

pub(crate) fn benchmark_report_dir_with_document(
    report_dir: &Path,
    options: BenchmarkOptions,
) -> (i32, Option<crate::agentskills::benchmark::BenchmarkDocument>) {
    let document = match build_benchmark(report_dir, options) {
        Ok(document) => document,
        Err(e) => {
            eprintln!("Failed to build benchmark: {}", e);
            return (1, None);
        }
    };

    if let Err(e) = write_benchmark(report_dir, &document) {
        eprintln!("Failed to write benchmark.json: {}", e);
        return (1, None);
    }

    (0, Some(document))
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
    fn benchmark_warns_on_eval_suite_drift() {
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
                "benchmark",
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
    }

    #[test]
    fn benchmark_allow_eval_suite_drift_suppresses_warning() {
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
                "benchmark",
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
        assert_eq!(json["warnings"][0]["kind"], "eval_suite_drift");
    }

    #[test]
    fn benchmark_help_documents_allow_eval_suite_drift() {
        let help = BenchmarkArgs::augment_args(clap::Command::new("benchmark").about("benchmark"))
            .render_long_help()
            .to_string();
        assert!(help.contains("--allow-eval-suite-drift"));
    }
}
