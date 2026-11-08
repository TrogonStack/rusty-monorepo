use super::testutil::{sample_prior_iteration_fixture, write_empty_feedback};

use super::*;
use crate::agentskills::report::ScenarioKind;
use crate::fs::FileSystem;

fn prior_report(temp: &tempfile::TempDir) -> ReportDocument {
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(temp, &skill_root);
    let content = std::fs::read_to_string(report_dir.join("report.json")).unwrap();
    serde_json::from_str(&content).unwrap()
}

/// `--skill-dir` falls back to the path the prior report recorded, and the suite
/// directory has to fall back the same way. A run made under a custom suite directory
/// would otherwise be drift-checked against `evals`, which is a different suite or no
/// suite at all, so the check reports drift that is not there or misses drift that is.
#[test]
fn an_unnamed_eval_dir_is_read_back_from_the_iteration_being_improved_on() {
    let temp = tempfile::tempdir().unwrap();
    let mut report = prior_report(&temp);
    report.suite.eval_dir = EvalDirName::parse("evals-nightly").unwrap();

    assert_eq!(
        eval_dir_for_drift_check(None, &report).as_str(),
        "evals-nightly",
        "the suite the prior iteration ran is the one to check against"
    );
}

#[test]
fn a_named_eval_dir_wins_over_the_one_the_prior_iteration_used() {
    let temp = tempfile::tempdir().unwrap();
    let mut report = prior_report(&temp);
    report.suite.eval_dir = EvalDirName::parse("evals-nightly").unwrap();
    let named = EvalDirName::parse("evals-fast").unwrap();

    assert_eq!(eval_dir_for_drift_check(Some(&named), &report).as_str(), "evals-fast");
}

#[test]
fn bundle_includes_sections_counts_and_transcript_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let output = write_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert!(output.markdown_path.is_file());
    assert!(output.json_path.is_file());
    assert_eq!(output.output_dir, report_dir.parent().unwrap().join(NEXT_ITERATION_DIR));

    let markdown = std::fs::read_to_string(output.markdown_path).unwrap();
    for section in [
        "## Summary",
        "## Failed Assertions",
        "## Human Feedback",
        "## Transcript Excerpts",
        "## Suggested Focus",
    ] {
        assert!(markdown.contains(section), "missing {section}");
    }

    assert_eq!(output.document.failed_assertions.len(), 2);
    assert_eq!(output.document.human_feedback.len(), 1);
    assert_eq!(output.document.transcript_excerpts.len(), 2);
    assert!(output.document.transcript_excerpts[0].truncated);
    assert_eq!(output.document.transcript_excerpts[0].head.len(), DEFAULT_EXCERPT_LINES);
    assert_eq!(output.document.summary.total_runs, 4);
    assert_eq!(output.document.summary.failed_assertions, 2);
    assert!(!output.document.eval_suite_drift.detected);
}

#[test]
fn bundle_warns_on_eval_suite_drift() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    std::fs::write(
            skill_root.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "changed prompt",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }, { "type": "contains", "text": "assert a2" }]
                    }
                ]
            }"#,
        )
        .unwrap();

    let output = write_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert!(output.document.eval_suite_drift.detected);
    assert!(output
        .document
        .eval_suite_drift
        .warning
        .as_deref()
        .unwrap_or("")
        .contains("eval suite changed between iterations"));

    let markdown = std::fs::read_to_string(output.markdown_path).unwrap();
    assert!(markdown.contains("WARN: eval suite changed between iterations"));
}

#[test]
fn suggested_focus_flags_underperforming_with_skill() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let output = write_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert!(output
        .document
        .suggested_focus
        .iter()
        .any(|item| item.contains("case-a") && item.contains("with_skill")));
}

#[test]
fn bundle_attaches_matching_feedback_to_failed_assertion_entry() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    let case_a = document
        .failed_assertions
        .iter()
        .find(|group| group.eval_case_id == "case-a")
        .expect("case-a failures");
    let failure = case_a
        .failures
        .iter()
        .find(|failure| failure.run_id == "run-001")
        .expect("run-001 failure");
    let feedback = failure
        .human_feedback
        .as_ref()
        .expect("feedback attached to failed run");

    assert_eq!(feedback.source_path, "runs/run-001/feedback.json");
    assert_eq!(feedback.notes.len(), 1);
    assert!(feedback.notes[0].text.contains("output path guidance"));

    let grouped = document
        .human_feedback
        .iter()
        .find(|entry| entry.run_id == "run-001")
        .expect("grouped feedback for run-001");
    assert_eq!(grouped.eval_case_id, "case-a");
    assert_eq!(grouped.scenario_id, ScenarioKind::WithSkill);
    assert_eq!(grouped.feedback.source_path, feedback.source_path);
}

#[test]
fn bundle_includes_feedback_without_failed_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    testutil::write_feedback(
        &report_dir.join("runs/run-002"),
        "Skill prompt should mention CSV headers explicitly",
    );

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert!(
        document
            .failed_assertions
            .iter()
            .flat_map(|group| &group.failures)
            .all(|failure| failure.run_id != "run-002"),
        "run-002 has no failed assertions"
    );

    let entry = document
        .human_feedback
        .iter()
        .find(|entry| entry.run_id == "run-002")
        .expect("feedback-only run appears in bundle");
    assert_eq!(entry.eval_case_id, "case-a");
    assert_eq!(entry.scenario_id, ScenarioKind::WithoutSkill);
    assert_eq!(entry.feedback.source_path, "runs/run-002/feedback.json");
}

#[test]
fn bundle_omits_empty_feedback_but_notes_clean_reviews() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    write_empty_feedback(&report_dir.join("runs/run-002"));

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert_eq!(document.human_feedback.len(), 1);
    assert!(
        document.human_feedback.iter().all(|entry| entry.run_id != "run-002"),
        "empty feedback must not create a bundle entry"
    );
    assert_eq!(document.human_feedback_summary.reviewed_no_issues_runs, 1);

    let markdown = render_improvement_bundle_markdown(&document);
    assert!(markdown.contains("reviewed with no issues"));
    assert!(!markdown.contains("run-002"));
}

#[test]
fn bundle_withholds_test_split_content_and_reports_only_its_pass_rate() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();
    let json = serde_json::to_string_pretty(&document).unwrap();
    let markdown = render_improvement_bundle_markdown(&document);

    for leak in [
        "TESTLEAK-PROMPT-C",
        "TESTLEAK-EVIDENCE-C",
        "TESTLEAK-FEEDBACK-NOTE",
        "TESTLEAK-TRANSCRIPT-MARKER",
        "case-c-held-out",
        "run-005",
        "run-006",
    ] {
        assert!(!json.contains(leak), "bundle JSON leaked held-out content: {leak}");
        assert!(
            !markdown.contains(leak),
            "bundle markdown leaked held-out content: {leak}"
        );
    }

    match &document.held_out {
        HeldOutSection::Withheld {
            test_case_count,
            test_run_count,
            scenarios,
            ..
        } => {
            assert_eq!(*test_case_count, 1);
            assert_eq!(*test_run_count, 2);
            assert!(!scenarios.is_empty());
        }
        other => panic!("expected a withheld test case, got {other:?}"),
    }
    assert!(markdown.contains("Held-Out Test Cases"));
    assert!(markdown.contains("withheld"));
}

#[test]
fn bundle_nudges_toward_declaring_test_cases_when_none_exist() {
    let temp = tempfile::tempdir().unwrap();
    let fs = crate::fs::testutil::MemFS::new();
    let skill_path = Path::new("no-test-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: no-test-skill\ndescription: d\n---\n",
    );
    fs.insert(
        skill_path.join("evals/evals.json"),
        r#"{
                "skill_name": "no-test-skill",
                "evals": [
                    {
                        "id": "only-case",
                        "prompt": "prompt",
                        "expected_output": "output",
                        "graders": [{ "type": "contains", "text": "assert" }]
                    }
                ]
            }"#,
    );

    let bundle = crate::agentskills::report::build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "no-test-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        crate::agentskills::report::BuildReportOptions {
            report_id: Some("report-iter-1".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            iteration: Some(1),
            ..crate::agentskills::report::BuildReportOptions::default()
        },
    )
    .unwrap();
    let report_dir = crate::agentskills::report::write_report_bundle(
        temp.path(),
        &bundle,
        crate::agentskills::report::WriteReportOptions::default(),
    )
    .unwrap();

    let skill_root = temp.path().join("current-skill");
    std::fs::create_dir_all(skill_root.join("evals")).unwrap();
    std::fs::write(
        skill_root.join("evals/evals.json"),
        fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        skill_root.join("SKILL.md"),
        fs.read_to_string(&skill_path.join("SKILL.md")).unwrap(),
    )
    .unwrap();

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert_eq!(document.held_out, HeldOutSection::NoTestCases);
    let markdown = render_improvement_bundle_markdown(&document);
    assert!(markdown.contains("Held-Out Test Cases"));
    assert!(markdown.contains("Declare at least one case"));
}

/// Builds a report narrowed to the `train` split of a two-case suite (one `train`, one
/// `test`), so the `test`-split case is declared but never run, the shape `--split
/// train` produces on a real suite.
fn train_only_report_with_a_declared_held_out_case(temp: &tempfile::TempDir, skill_root: &Path) -> PathBuf {
    let fs = crate::fs::testutil::MemFS::new();
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
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    },
                    {
                        "id": "case-b-held-out",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "graders": [{ "type": "contains", "text": "assert b" }],
                        "split": "test"
                    }
                ]
            }"#,
    );

    let bundle = crate::agentskills::report::build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        crate::agentskills::report::BuildReportOptions {
            report_id: Some("report-iter-1".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            iteration: Some(1),
            cases: crate::agentskills::case_selection::CaseSelection::parse(&[], &[], Some(EvalSplit::Train)).unwrap(),
            ..crate::agentskills::report::BuildReportOptions::default()
        },
    )
    .unwrap();
    let report_dir = crate::agentskills::report::write_report_bundle(
        temp.path(),
        &bundle,
        crate::agentskills::report::WriteReportOptions::default(),
    )
    .unwrap();

    std::fs::create_dir_all(skill_root.join("evals")).unwrap();
    std::fs::write(
        skill_root.join("evals/evals.json"),
        fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        skill_root.join("SKILL.md"),
        fs.read_to_string(&skill_path.join("SKILL.md")).unwrap(),
    )
    .unwrap();

    report_dir
}

#[test]
fn a_narrowed_run_does_not_leak_a_never_run_held_out_case_as_removed() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = train_only_report_with_a_declared_held_out_case(&temp, &skill_root);

    // The current suite on disk drops the held-out case entirely, so its id can only be
    // told apart from a real removal by consulting what the prior run declared, not what
    // it ran.
    std::fs::write(
        skill_root.join("evals/evals.json"),
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a, reworded",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    }
                ]
            }"#,
    )
    .unwrap();

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert!(document.eval_suite_drift.detected);
    assert!(
        !document
            .eval_suite_drift
            .removed_eval_ids
            .iter()
            .any(|id| id == "case-b-held-out"),
        "a held-out case excluded by --split train must not leak into the improvement \
             bundle: {:?}",
        document.eval_suite_drift.removed_eval_ids
    );
}

#[test]
fn held_out_section_tells_a_narrowed_selection_from_a_suite_with_no_test_cases() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = train_only_report_with_a_declared_held_out_case(&temp, &skill_root);

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        document.held_out,
        HeldOutSection::DeclaredNotRun {
            declared_test_case_count: 1
        },
        "the suite declares a held-out case, this run just did not cover it"
    );

    let markdown = render_improvement_bundle_markdown(&document);
    assert!(markdown.contains("declares 1 `test`-split case"));
    assert!(!markdown.contains("declares no `test`-split cases"));
}
