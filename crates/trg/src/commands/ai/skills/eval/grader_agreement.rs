use std::path::PathBuf;

use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::grader_agreement::{
    build_grader_agreement_document, write_grader_agreement, AgreementBucket, AgreementThreshold,
    GraderAgreementDocument,
};
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval grader-agreement ./report

  $ trg ai skills eval grader-agreement ./report --output-format json

  $ trg ai skills eval grader-agreement ./report --min-agreement 0.9
")]
pub struct GraderAgreementArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "PROPORTION",
        help = "Fail when the LLM-grader agreement interval's Wilson lower bound is below this proportion in (0, 1]"
    )]
    pub min_agreement: Option<AgreementThreshold>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as the grader-agreement.json document on stdout"
    )]
    pub output_format: OutputFormat,
}

impl GraderAgreementArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let document = match build_grader_agreement_document(&self.report_dir) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("Failed to build grader agreement report: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(error) = write_grader_agreement(&self.report_dir, &document) {
            eprintln!("Failed to write grader-agreement.json: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            match serde_json::to_string_pretty(&document) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize grader agreement report: {error}");
                    return ExitCode::InfrastructureFailure;
                }
            }
        } else {
            print_human_summary(&document);
            print_report_dir(&self.report_dir);
        }

        let Some(threshold) = self.min_agreement else {
            return ExitCode::Success;
        };

        match evaluate_gate(&document, threshold) {
            Ok(()) => ExitCode::Success,
            Err(message) => {
                eprintln!("grader-agreement: {message}");
                ExitCode::GateFailed
            }
        }
    }
}

/// Whether the `llm` bucket's agreement floor clears `threshold`.
///
/// Checks the `llm` bucket specifically, not the overall rate: an LLM judge is the one
/// grader kind whose verdicts are not already deterministic given its inputs, so it is
/// the one worth gating a CI run on. A threshold nobody's labels can answer yet, because
/// no llm-graded assertion has a human verdict, is reported as a gate failure rather than
/// a silent pass, so `--min-agreement` never reads as checked when it checked nothing.
fn evaluate_gate(document: &GraderAgreementDocument, threshold: AgreementThreshold) -> Result<(), String> {
    let Some(interval) = document.by_grader_kind.llm.interval else {
        return Err(format!(
            "no llm-graded assertion has a human label yet, so --min-agreement {threshold} could not be checked"
        ));
    };

    if interval.low() < threshold.value() {
        return Err(format!(
            "llm grader agreement's Wilson lower bound {:.3} is below --min-agreement {threshold}",
            interval.low()
        ));
    }

    Ok(())
}

fn print_human_summary(document: &GraderAgreementDocument) {
    println!("Grader agreement:");
    print_bucket_line("  overall", &document.overall);
    print_bucket_line("  mechanical", &document.by_grader_kind.mechanical);
    print_bucket_line("  declarative", &document.by_grader_kind.declarative);
    print_bucket_line("  script", &document.by_grader_kind.script);
    print_bucket_line("  llm", &document.by_grader_kind.llm);

    println!(
        "  coverage: {}/{} graded assertions labelled ({} unlabelled)",
        document.coverage.labeled_assertions,
        document.coverage.graded_assertions,
        document.coverage.unlabeled.len()
    );

    if !document.disagreements.is_empty() {
        println!("  disagreements:");
        for disagreement in &document.disagreements {
            println!(
                "    {} (run {}): grader said {}, human said {}",
                disagreement.assertion_id, disagreement.run_id, disagreement.grader_passed, disagreement.human_verdict
            );
        }
    }
}

fn print_bucket_line(label: &str, bucket: &AgreementBucket) {
    match bucket.interval {
        Some(interval) => println!(
            "{label}: {}/{} agree, 95% CI [{:.3}, {:.3}]",
            bucket.agreements,
            bucket.agreements + bucket.disagreements,
            interval.low(),
            interval.high()
        ),
        None => println!("{label}: no labelled assertions"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::feedback::{record_assertion_verdict, HumanVerdict};
    use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use std::path::Path;

    fn sample_report_dir(temp: &tempfile::TempDir) -> PathBuf {
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
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "graders": [{ "type": "llm", "criterion": "assert a" }]
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
                report_id: Some("report-test".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap()
    }

    fn grade_run(report_dir: &Path, run_id: &str, passed: bool) {
        let result = AssertionGradeResult {
            assertion: "assert a".to_string(),
            passed,
            evidence: "the transcript said so".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Llm,
                model: Some("test-model".to_string()),
                command: None,
            },
            name: None,
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        };
        let grading = build_grading_file(vec![result]).unwrap();
        std::fs::write(
            report_dir.join(format!("runs/{run_id}/grading.json")),
            serde_json::to_string_pretty(&grading).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn command_writes_the_artifact_and_passes_without_a_gate() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_run(&report_dir, "run-001", true);
        record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Pass, None, Some("h")).unwrap();

        let status = GraderAgreementArgs {
            report_dir: report_dir.clone(),
            min_agreement: None,
            output_format: OutputFormat::Text,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, ExitCode::Success);
        assert!(report_dir.join("grader-agreement.json").is_file());
    }

    #[test]
    fn a_gate_with_no_llm_labels_yet_fails_rather_than_passing_silently() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_run(&report_dir, "run-001", true);

        let status = GraderAgreementArgs {
            report_dir,
            min_agreement: Some(AgreementThreshold::parse(0.9).unwrap()),
            output_format: OutputFormat::Text,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, ExitCode::GateFailed);
    }

    #[test]
    fn a_confidently_wrong_judge_fails_the_gate() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_run(&report_dir, "run-001", true);
        record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Fail, None, Some("h")).unwrap();

        let status = GraderAgreementArgs {
            report_dir,
            min_agreement: Some(AgreementThreshold::parse(0.5).unwrap()),
            output_format: OutputFormat::Text,
        }
        .handle(&crate::fs::RealFS);

        assert_eq!(status, ExitCode::GateFailed);
    }
}
