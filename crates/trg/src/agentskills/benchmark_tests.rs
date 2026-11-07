use super::*;
use crate::agentskills::iteration_summary::{build_iteration_summary_document, IterationSummaryOptions};
use std::fs;

fn write_report(report_dir: &Path, runs: serde_json::Value, iteration: Option<serde_json::Value>) {
    fs::create_dir_all(report_dir).unwrap();
    let mut report = serde_json::json!({
        "report": {
            "id": "report-test",
            "generated_at": "2026-05-26T00:00:00Z",
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": "sha256:def"
        },
        "dimensions": {
            "eval_cases": [],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": runs,
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    if let Some(iteration) = iteration {
        report["report"]["iteration"] = iteration;
    }
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

fn write_run_artifacts(report_dir: &Path, run_id: &str, grading: Option<&str>, timing: Option<&str>) {
    let run_dir = report_dir.join(format!("runs/{run_id}"));
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    if let Some(grading) = grading {
        fs::write(run_dir.join("grading.json"), grading).unwrap();
    }
    if let Some(timing) = timing {
        fs::write(run_dir.join("timing.json"), timing).unwrap();
    }
}

fn sample_run(id: &str, scenario: &str, status: &str, iteration_id: Option<&str>) -> serde_json::Value {
    let mut run = serde_json::json!({
        "id": id,
        "eval_case_id": "case-a",
        "scenario_id": scenario,
        "model_config_id": "ci-default",
        "skill_revision_id": "current",
        "attempt": 1,
        "status": status,
        "paths": { "workspace": format!("runs/{id}/workspace") },
        "artifacts": [],
        "metrics": {}
    });
    if let Some(iteration_id) = iteration_id {
        run["iteration_id"] = serde_json::json!(iteration_id);
    }
    run
}

fn with_skill_run(id: &str, eval_case_id: &str, split: &str, attempt: u32) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "eval_case_id": eval_case_id,
        "split": split,
        "scenario_id": "with_skill",
        "model_config_id": "ci-default",
        "skill_revision_id": "current",
        "attempt": attempt,
        "status": "completed",
        "paths": { "workspace": format!("runs/{id}/workspace") },
        "artifacts": [],
        "metrics": {}
    })
}

#[test]
fn build_benchmark_reports_headroom_and_names_saturated_cases_per_split() {
    let temp = tempfile::tempdir().unwrap();
    let mut runs = Vec::new();
    for attempt in 1..=40 {
        runs.push(with_skill_run(&format!("case-a-{attempt}"), "case-a", "test", attempt));
        runs.push(with_skill_run(&format!("case-b-{attempt}"), "case-b", "train", attempt));
    }
    write_report(temp.path(), serde_json::Value::Array(runs), None);
    for attempt in 1..=40 {
        for case in ["case-a", "case-b"] {
            write_run_artifacts(
                temp.path(),
                &format!("{case}-{attempt}"),
                Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
                None,
            );
        }
    }

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

    let headroom = benchmark
        .headroom
        .expect("a fully saturated with_skill arm should warn");
    assert!(headroom.lower_bound >= HeadroomThreshold::default().value());
    assert_eq!(
        headroom.saturated_case_ids,
        vec!["case-a".to_string(), "case-b".to_string()]
    );

    let test_headroom = benchmark.by_split[&EvalSplit::Test]
        .headroom
        .as_ref()
        .expect("the test split alone is also saturated");
    assert_eq!(test_headroom.saturated_case_ids, vec!["case-a".to_string()]);

    let train_headroom = benchmark.by_split[&EvalSplit::Train]
        .headroom
        .as_ref()
        .expect("the train split alone is also saturated");
    assert_eq!(train_headroom.saturated_case_ids, vec!["case-b".to_string()]);
}

#[test]
fn build_benchmark_reports_no_headroom_warning_when_the_with_skill_arm_still_has_room() {
    let temp = tempfile::tempdir().unwrap();
    let mut runs = Vec::new();
    for attempt in 1..=10 {
        runs.push(with_skill_run(&format!("case-a-{attempt}"), "case-a", "test", attempt));
    }
    write_report(temp.path(), serde_json::Value::Array(runs), None);
    for attempt in 1..=10 {
        let passed = attempt <= 5;
        write_run_artifacts(
            temp.path(),
            &format!("case-a-{attempt}"),
            Some(&format!(
                r#"{{"assertion_results":[{{"assertion":"a","passed":{passed}}}]}}"#
            )),
            None,
        );
    }

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

    assert!(benchmark.headroom.is_none());
    assert!(benchmark.by_split[&EvalSplit::Test].headroom.is_none());
}

#[test]
fn build_benchmark_headroom_threshold_is_configurable() {
    let temp = tempfile::tempdir().unwrap();
    let mut runs = Vec::new();
    for attempt in 1..=5 {
        runs.push(with_skill_run(&format!("case-a-{attempt}"), "case-a", "test", attempt));
    }
    write_report(temp.path(), serde_json::Value::Array(runs), None);
    for attempt in 1..=5 {
        write_run_artifacts(
            temp.path(),
            &format!("case-a-{attempt}"),
            Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
            None,
        );
    }

    let default_threshold = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    assert!(
        default_threshold.headroom.is_none(),
        "5/5 does not clear the default 0.9 floor"
    );

    let lenient = build_benchmark(
        temp.path(),
        BenchmarkOptions {
            headroom_threshold: HeadroomThreshold::parse(0.5).unwrap(),
            ..BenchmarkOptions::default()
        },
    )
    .unwrap();
    assert!(lenient.headroom.is_some(), "the same arm clears a lower threshold");
}

#[test]
fn read_report_iteration_accepts_numeric_and_object_forms() {
    let temp = tempfile::tempdir().unwrap();

    write_report(temp.path(), serde_json::json!([]), Some(serde_json::json!(3)));
    assert_eq!(read_report_iteration(temp.path()).unwrap(), 3);

    write_report(
        temp.path(),
        serde_json::json!([]),
        Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
    );
    assert_eq!(read_report_iteration(temp.path()).unwrap(), 2);

    write_report(
        temp.path(),
        serde_json::json!([]),
        Some(serde_json::json!({ "id": "iter-4", "iteration": 4 })),
    );
    assert_eq!(read_report_iteration(temp.path()).unwrap(), 4);
}

#[test]
fn write_benchmark_mirrors_to_iteration_dir_for_object_iteration() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
        Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    write_benchmark(temp.path(), &benchmark).unwrap();

    assert!(temp.path().join("benchmark.json").is_file());
    assert!(temp.path().join("iteration-2/benchmark.json").is_file());
}

#[test]
fn duration_stats_omit_stddev_for_small_samples() {
    let two = duration_stats(&[100, 200]);
    assert!(two.stddev.is_none());

    let three = duration_stats(&[100, 200, 300]);
    assert!(three.stddev.is_some());
}

#[test]
fn duration_stats_withhold_a_spread_below_the_dispersion_floor() {
    let three = duration_stats(&[100, 200, 300]);
    assert!(
        three.median.is_none(),
        "three draws cannot describe their own spread yet"
    );

    let four = duration_stats(&[100, 200, 300, 400]);
    assert_eq!(four.median, Some(250.0));
    assert_eq!(four.mad, Some(100.0));
}

#[test]
fn percentile_and_stddev_helpers() {
    assert_eq!(percentile(&[10, 20], 0.50), 15);
    assert_eq!(percentile(&[10, 20, 30, 40], 0.50), 25);
    assert_eq!(percentile(&[10, 20, 30, 40], 0.95), 39);
    assert!((stddev(&[2, 4, 6], 4.0) - 1.632993161855452).abs() < 0.0001);
}

#[test]
fn aggregates_pass_rates_and_missing_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "without_skill", "completed", None),
        ]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 2, "failed": 0, "total": 2, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000, "total_tokens": 100, "input_tokens": 60, "output_tokens": 40 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-002",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": false, "evidence": "missing" }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
        ),
        None,
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let with_skill = &benchmark.scenarios["with_skill"];
    assert_eq!(with_skill.completed.assertions.pass_rate, 1.0);
    assert_eq!(with_skill.completed.missing_timing, 0);
    assert_eq!(with_skill.completed.missing_grading, 0);

    let without_skill = &benchmark.scenarios["without_skill"];
    assert_eq!(without_skill.completed.assertions.pass_rate, 0.5);
    assert_eq!(without_skill.completed.missing_timing, 1);

    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
    assert!((delta.assertion_pass_rate - 0.5).abs() < 0.0001);

    let interval = delta
        .assertion_pass_rate_interval
        .expect("both arms scored assertions, so the difference has a width");
    assert!(
        !interval.separates_the_arms(),
        "two assertions against two read as a finding at {interval:?}"
    );
}

#[test]
fn a_difference_against_an_arm_that_scored_nothing_publishes_no_interval() {
    let left = PassFailSummary {
        passed: 2,
        failed: 0,
        total: 2,
        pass_rate: 1.0,
    };
    let nothing = PassFailSummary {
        passed: 0,
        failed: 0,
        total: 0,
        pass_rate: 0.0,
    };

    assert!(difference_interval(&left, &nothing).is_none());
    assert!(difference_interval(&left, &left).is_some());
}

#[test]
fn failed_runs_bucket_exclude_and_zero_modes() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "with_skill", "failed", None),
        ]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );
    write_run_artifacts(temp.path(), "run-002", None, Some(r#"{ "duration_ms": 500 }"#));

    let bucket = build_benchmark(
        temp.path(),
        BenchmarkOptions {
            failed_runs: FailedRunsMode::Bucket,
            ..BenchmarkOptions::default()
        },
    )
    .unwrap();
    assert_eq!(bucket.scenarios["with_skill"].completed.run_count, 1);
    assert_eq!(bucket.scenarios["with_skill"].failed.as_ref().unwrap().run_count, 1);

    let exclude = build_benchmark(
        temp.path(),
        BenchmarkOptions {
            failed_runs: FailedRunsMode::Exclude,
            ..BenchmarkOptions::default()
        },
    )
    .unwrap();
    assert_eq!(exclude.scenarios["with_skill"].completed.run_count, 1);
    assert!(exclude.scenarios["with_skill"].failed.is_none());

    let zero = build_benchmark(
        temp.path(),
        BenchmarkOptions {
            failed_runs: FailedRunsMode::Zero,
            ..BenchmarkOptions::default()
        },
    )
    .unwrap();
    assert_eq!(zero.scenarios["with_skill"].completed.run_count, 2);
    assert_eq!(zero.scenarios["with_skill"].completed.runs.passed, 1);
    assert_eq!(zero.scenarios["with_skill"].completed.runs.failed, 1);
    assert_eq!(zero.scenarios["with_skill"].completed.missing_grading, 0);
}

#[test]
fn unparseable_grading_counts_as_missing() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some("{ this is not valid json"),
        Some(r#"{ "duration_ms": 1000 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let with_skill = &benchmark.scenarios["with_skill"];
    assert_eq!(with_skill.completed.run_count, 1);
    assert_eq!(with_skill.completed.missing_grading, 1);
    assert_eq!(with_skill.completed.runs.passed, 0);
    assert_eq!(with_skill.completed.runs.failed, 0);
}

#[test]
fn mixed_completed_failed_skipped_and_missing_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "with_skill", "failed", None),
            sample_run("run-003", "with_skill", "skipped", None),
            sample_run("run-004", "without_skill", "completed", None),
        ]),
        None,
    );
    write_run_artifacts(temp.path(), "run-001", None, Some(r#"{ "duration_ms": 1000 }"#));
    write_run_artifacts(
        temp.path(),
        "run-002",
        None,
        Some(r#"{ "duration_ms": 2000, "total_tokens": 50 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-004",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "a", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 3000 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let with_skill = &benchmark.scenarios["with_skill"];
    assert_eq!(with_skill.completed.run_count, 1);
    assert_eq!(with_skill.completed.missing_grading, 1);
    assert_eq!(with_skill.failed.as_ref().unwrap().run_count, 1);
    assert_eq!(with_skill.skipped.run_count, 1);

    let without_skill = &benchmark.scenarios["without_skill"];
    assert_eq!(without_skill.completed.runs.failed, 1);
}

#[test]
fn an_arm_scoped_check_does_not_widen_the_gap_between_the_arms() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "without_skill", "completed", None),
        ]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "the skill was engaged", "passed": true, "evidence": "read SKILL.md", "excluded": "presupposes the skill" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 2, "excluded": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-002",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "the skill was engaged", "passed": false, "evidence": "never read", "excluded": "presupposes the skill" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 2, "excluded": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
    assert!(
        delta.assertion_pass_rate.abs() < 0.0001,
        "the skill was credited for its own premise: {}",
        delta.assertion_pass_rate
    );
    assert!(benchmark.iteration_summary.always_fail.is_empty());
}

#[test]
fn iteration_comparison_when_metadata_present() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", Some("iter-1")),
            sample_run("run-002", "with_skill", "completed", Some("iter-2")),
        ]),
        Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-002",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 2, "failed": 0, "total": 2, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 800 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let comparison = benchmark.iteration_comparison.as_ref().unwrap();
    assert_eq!(comparison.current_iteration_id, "iter-2");
    assert_eq!(comparison.previous_iteration_id, "iter-1");
    let delta = comparison.by_scenario.get("with_skill").unwrap();
    assert!((delta.assertion_pass_rate - 0.0).abs() < 0.0001);
    assert!((delta.duration_ms_mean - (-200.0)).abs() < 0.0001);
    assert_eq!(delta.left.runs, 1);
    assert_eq!(delta.right.runs, 1);
}

#[test]
fn a_scenario_delta_carries_how_many_runs_stood_behind_each_arm() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "with_skill", "completed", None),
            sample_run("run-003", "without_skill", "completed", None),
        ]),
        None,
    );
    for run_id in ["run-001", "run-002", "run-003"] {
        write_run_artifacts(
            temp.path(),
            run_id,
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );
    }

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
    assert_eq!(delta.left.runs, 2);
    assert_eq!(delta.right.runs, 1);
    assert!(
        delta.left.duration_median_ms.is_none(),
        "two draws is below the floor a spread needs to describe itself"
    );
}

fn write_arms_with_timing(report_dir: &Path, arms: &[(&str, Option<&str>, Vec<String>)]) {
    let mut runs = Vec::new();
    let mut artifacts = Vec::new();
    for (scenario, iteration_id, timings) in arms {
        for timing in timings {
            let run_id = format!("run-{:03}", runs.len() + 1);
            runs.push(sample_run(&run_id, scenario, "completed", *iteration_id));
            artifacts.push((run_id, timing.clone()));
        }
    }
    write_report(report_dir, serde_json::Value::Array(runs), None);
    for (run_id, timing) in artifacts {
        write_run_artifacts(
            report_dir,
            &run_id,
            Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
            Some(&timing),
        );
    }
}

fn priced_timing(duration_ms: u64, input_tokens: u64, total_tokens: u64, usd: f64) -> String {
    format!(
        r#"{{ "duration_ms": {duration_ms}, "input_tokens": {input_tokens}, "total_tokens": {total_tokens}, "cost": {{ "kind": "priced", "usd": {usd} }} }}"#
    )
}

#[test]
fn a_scenario_delta_says_how_far_the_runs_of_two_arms_sit_apart() {
    let temp = tempfile::tempdir().unwrap();
    let baseline: Vec<String> = [1000, 1100, 1200, 1300, 1050]
        .into_iter()
        .map(|duration| priced_timing(duration, 400, 500, 0.10))
        .collect();
    let candidate: Vec<String> = [2000, 2100, 2200, 2300, 2050]
        .into_iter()
        .map(|duration| priced_timing(duration, 800, 1000, 0.30))
        .collect();
    write_arms_with_timing(
        temp.path(),
        &[("with_skill", None, candidate), ("without_skill", None, baseline)],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();

    let duration = delta.duration_ms_shift.expect("both arms cleared the floor");
    assert_eq!(duration.estimate(), 1000.0);
    assert!(duration.interval().low() > 0.0, "{:?}", duration.interval());
    let cost = delta.cost_usd_per_run_shift.expect("both arms priced every run");
    assert!((cost.estimate() - 0.20).abs() < 1e-9);
    assert!((delta.input_tokens_per_run_shift.unwrap().estimate() - 400.0).abs() < 1e-9);
    assert!((delta.total_tokens_per_run_shift.unwrap().estimate() - 500.0).abs() < 1e-9);
}

#[test]
fn a_scenario_delta_publishes_no_shift_for_an_arm_below_the_floor() {
    let temp = tempfile::tempdir().unwrap();
    let three: Vec<String> = (0..3).map(|_| priced_timing(1000, 400, 500, 0.10)).collect();
    let five: Vec<String> = (0..5).map(|_| priced_timing(2000, 800, 1000, 0.30)).collect();
    write_arms_with_timing(
        temp.path(),
        &[("with_skill", None, five), ("without_skill", None, three)],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();

    assert!(delta.duration_ms_shift.is_none());
    assert!(delta.cost_usd_per_run_shift.is_none());
    assert!(delta.input_tokens_per_run_shift.is_none());
    assert!(delta.total_tokens_per_run_shift.is_none());
    let published = serde_json::to_value(delta).unwrap();
    for field in [
        "duration_ms_shift",
        "cost_usd_per_run_shift",
        "input_tokens_per_run_shift",
        "total_tokens_per_run_shift",
    ] {
        assert!(published.get(field).is_none(), "{field} was published as {published}");
    }
}

#[test]
fn a_per_run_cost_shift_is_withheld_unless_both_arms_priced_every_run() {
    let temp = tempfile::tempdir().unwrap();
    let mut candidate: Vec<String> = (0..4).map(|_| priced_timing(2000, 800, 1000, 0.30)).collect();
    candidate.push(r#"{ "duration_ms": 2000, "input_tokens": 800, "total_tokens": 1000 }"#.to_string());
    let baseline: Vec<String> = (0..5).map(|_| priced_timing(1000, 400, 500, 0.10)).collect();
    write_arms_with_timing(
        temp.path(),
        &[("with_skill", None, candidate), ("without_skill", None, baseline)],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();

    assert!(delta.cost_usd_per_run_shift.is_none());
    assert!(delta.duration_ms_shift.is_some());
}

#[test]
fn an_iteration_comparison_carries_the_same_per_run_shifts() {
    let temp = tempfile::tempdir().unwrap();
    let previous: Vec<String> = [1000, 1100, 1200, 1300]
        .into_iter()
        .map(|duration| priced_timing(duration, 400, 500, 0.10))
        .collect();
    let current: Vec<String> = [3000, 3100, 3200, 3300]
        .into_iter()
        .map(|duration| priced_timing(duration, 400, 500, 0.10))
        .collect();
    write_arms_with_timing(
        temp.path(),
        &[
            ("with_skill", Some("iteration-1"), previous),
            ("with_skill", Some("iteration-2"), current),
        ],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let comparison = benchmark.iteration_comparison.expect("two iterations ran");
    let delta = &comparison.by_scenario["with_skill"];

    let duration = delta.duration_ms_shift.expect("both iterations cleared the floor");
    assert_eq!(duration.estimate(), 2000.0);
    assert!(duration.interval().low() > 0.0);
    let cost = delta.cost_usd_per_run_shift.expect("every run was priced");
    assert_eq!(cost.estimate(), 0.0);
    assert!(!cost.interval().separates_the_arms());
}

#[test]
fn flaky_assertions_detect_pass_flip_across_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let mut run1 = sample_run("run-001", "with_skill", "completed", None);
    let mut run2 = sample_run("run-002", "with_skill", "completed", None);
    run1["eval_case_id"] = serde_json::json!("case-a");
    run2["eval_case_id"] = serde_json::json!("case-a");
    run1["attempt"] = serde_json::json!(1);
    run2["attempt"] = serde_json::json!(2);
    write_report(temp.path(), serde_json::json!([run1, run2]), None);
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "checks output", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1000 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-002",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "checks output", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1100 }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let row = benchmark
        .by_eval_scenario
        .iter()
        .find(|row| row.eval_case_id == "case-a")
        .unwrap();
    assert_eq!(row.attempt_count, 2);
    assert_eq!(row.flaky_assertions, vec!["checks output"]);
    assert_eq!(benchmark.iteration_summary.flaky_assertions.len(), 1);
}

#[test]
fn pass_rate_variance_across_attempts() {
    let rates = [1.0, 0.5, 0.0];
    let mean = rates.iter().sum::<f64>() / rates.len() as f64;
    let variance = pass_rate_variance(&rates, mean);
    assert!((variance - 0.16666666666666666).abs() < 0.0001);

    let stats = attempt_pass_rate_stats(&rates);
    assert!((stats.mean - mean).abs() < 0.0001);
    assert!((stats.variance.unwrap() - variance).abs() < 0.0001);
    assert_eq!(stats.min, 0.0);
    assert_eq!(stats.max, 1.0);

    assert_eq!(pass_rate_variance(&[], 0.0), 0.0);
}

fn write_timed_runs(report_dir: &Path, timings: &[(&str, &str, u32, u64, u64)]) {
    let runs: Vec<serde_json::Value> = timings
        .iter()
        .map(|(id, scenario, attempt, _, _)| {
            let mut run = sample_run(id, scenario, "completed", None);
            run["attempt"] = serde_json::json!(attempt);
            run
        })
        .collect();
    write_report(report_dir, serde_json::json!(runs), None);
    for (id, _, _, duration_ms, total_tokens) in timings {
        write_run_artifacts(
            report_dir,
            id,
            None,
            Some(&format!(
                r#"{{ "duration_ms": {duration_ms}, "total_tokens": {total_tokens} }}"#
            )),
        );
    }
}

#[test]
fn a_group_below_the_sample_floor_reports_no_outlier() {
    let temp = tempfile::tempdir().unwrap();
    write_timed_runs(
        temp.path(),
        &[
            ("run-001", "with_skill", 1, 1000, 100),
            ("run-002", "with_skill", 2, 1200, 120),
            ("run-003", "with_skill", 3, 9000, 900),
        ],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    assert!(benchmark.iteration_summary.timing_outliers.is_empty());
    assert!(benchmark.iteration_summary.token_outliers.is_empty());
}

/// The arms pay different costs by construction, so a pooled centre sits between
/// them and publishes the slower arm as a field of outliers against the faster one.
#[test]
fn each_arm_is_judged_against_its_own_cost_profile() {
    let temp = tempfile::tempdir().unwrap();
    write_timed_runs(
        temp.path(),
        &[
            ("run-001", "with_skill", 1, 1000, 100),
            ("run-002", "with_skill", 2, 1020, 102),
            ("run-003", "with_skill", 3, 980, 98),
            ("run-004", "with_skill", 4, 1010, 101),
            ("run-005", "without_skill", 1, 29000, 2900),
            ("run-006", "without_skill", 2, 30000, 3000),
            ("run-007", "without_skill", 3, 30500, 3050),
            ("run-008", "without_skill", 4, 31000, 3100),
        ],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    assert!(benchmark.iteration_summary.timing_outliers.is_empty());
    assert!(benchmark.iteration_summary.token_outliers.is_empty());
}

#[test]
fn an_outlier_is_published_with_the_band_its_own_arm_drew() {
    let temp = tempfile::tempdir().unwrap();
    write_timed_runs(
        temp.path(),
        &[
            ("run-001", "with_skill", 1, 1000, 100),
            ("run-002", "with_skill", 2, 1020, 102),
            ("run-003", "with_skill", 3, 980, 98),
            ("run-004", "with_skill", 4, 1010, 101),
            ("run-005", "with_skill", 5, 9000, 900),
        ],
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let timing = &benchmark.iteration_summary.timing_outliers;
    assert_eq!(timing.len(), 1);
    assert_eq!(timing[0].run_id, "run-005");
    assert_eq!(timing[0].attempt, 5);
    assert_eq!(timing[0].value, 9000);
    assert_eq!(timing[0].median, 1010.0);
    assert_eq!(timing[0].mad, 10.0);

    let tokens = &benchmark.iteration_summary.token_outliers;
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, 900);
    assert_eq!(tokens[0].median, 101.0);
}

#[test]
fn benchmark_and_iteration_summary_agree_on_the_same_pass() {
    let temp = tempfile::tempdir().unwrap();
    let ids = ["run-001", "run-002", "run-003", "run-004", "run-005"];
    let durations = [1000_u64, 1020, 980, 1010, 9000];
    let tokens = [100_u64, 102, 98, 101, 900];
    let passed = [true, true, true, true, false];

    let runs: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let mut run = sample_run(id, "with_skill", "completed", None);
            run["attempt"] = serde_json::json!(index as u32 + 1);
            run
        })
        .collect();
    write_report(temp.path(), serde_json::json!(runs), Some(serde_json::json!(1)));
    for (index, id) in ids.iter().enumerate() {
        write_run_artifacts(
            temp.path(),
            id,
            Some(&format!(
                r#"{{ "assertion_results": [{{ "assertion": "finishes without error", "passed": {} }}] }}"#,
                passed[index]
            )),
            Some(&format!(
                r#"{{ "duration_ms": {}, "total_tokens": {} }}"#,
                durations[index], tokens[index]
            )),
        );
    }

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    let summary = build_iteration_summary_document(temp.path(), IterationSummaryOptions::default()).unwrap();

    assert!(!summary.flaky_assertions.is_empty());
    assert_eq!(benchmark.iteration_summary.flaky_assertions, summary.flaky_assertions);

    let benchmark_timing = &benchmark.iteration_summary.timing_outliers;
    let summary_timing = &summary.timing_outliers;
    assert_eq!(benchmark_timing.len(), 1);
    assert_eq!(summary_timing.len(), 1);
    assert_eq!(benchmark_timing[0].eval_case_id, summary_timing[0].eval_case_id);
    assert_eq!(benchmark_timing[0].scenario_id, summary_timing[0].scenario_id);
    assert_eq!(benchmark_timing[0].attempt, summary_timing[0].attempt);
    assert_eq!(benchmark_timing[0].value, summary_timing[0].duration_ms);
    assert_eq!(benchmark_timing[0].median, summary_timing[0].median_ms);
    assert_eq!(benchmark_timing[0].mad, summary_timing[0].mad_ms);

    let benchmark_tokens = &benchmark.iteration_summary.token_outliers;
    let summary_tokens = &summary.token_outliers;
    assert_eq!(benchmark_tokens.len(), 1);
    assert_eq!(summary_tokens.len(), 1);
    assert_eq!(benchmark_tokens[0].eval_case_id, summary_tokens[0].eval_case_id);
    assert_eq!(benchmark_tokens[0].scenario_id, summary_tokens[0].scenario_id);
    assert_eq!(benchmark_tokens[0].attempt, summary_tokens[0].attempt);
    assert_eq!(benchmark_tokens[0].value, summary_tokens[0].total_tokens);
    assert_eq!(benchmark_tokens[0].median, summary_tokens[0].median_tokens);
    assert_eq!(benchmark_tokens[0].mad, summary_tokens[0].mad_tokens);
}

/// A bucket of ten runs where one carried a price used to publish that one price as
/// the bucket's cost, which reads as nine runs that were free.
#[test]
fn a_bucket_only_some_of_whose_runs_were_priced_says_what_the_total_covers() {
    let costs = vec![RunCost::Priced { usd: 0.25 }];

    assert_eq!(bucket_cost(&costs, 3), Some(BucketCost::Partial { usd: 0.25, runs: 1 }));
}

#[test]
fn a_bucket_every_run_of_which_was_priced_reports_the_bucket_cost() {
    let costs = vec![RunCost::Priced { usd: 0.25 }, RunCost::Priced { usd: 0.75 }];

    assert_eq!(bucket_cost(&costs, 2), Some(BucketCost::Whole { usd: 1.0 }));
}

/// Totalling the rest would read the run its harness refuses to price as a run that
/// was free, so there is no total to publish here at all.
#[test]
fn a_bucket_holding_a_run_no_harness_prices_has_no_total() {
    let costs = vec![
        RunCost::Priced { usd: 0.25 },
        RunCost::Unpriced {
            harness: "codex".to_string(),
        },
    ];

    assert_eq!(
        bucket_cost(&costs, 2),
        Some(BucketCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}

#[test]
fn a_bucket_no_run_of_which_reported_anything_carries_no_cost() {
    assert_eq!(bucket_cost(&[], 3), None);
}

/// Only a bucket whose total is its own cost has a number to put under the name an
/// earlier release read as exactly that.
#[test]
fn only_a_whole_bucket_publishes_the_bare_number_earlier_releases_read() {
    let whole = TokenStats {
        cost: Some(BucketCost::Whole { usd: 0.25 }),
        ..TokenStats::default()
    };
    let partial = TokenStats {
        cost: Some(BucketCost::Partial { usd: 0.25, runs: 1 }),
        ..TokenStats::default()
    };

    assert_eq!(serde_json::to_value(&whole).unwrap()["cost_usd"], 0.25);
    assert!(serde_json::to_value(&partial).unwrap().get("cost_usd").is_none());
}

/// Subtracting a total that covers one arm from a total that covers part of another
/// answers no question anybody asked.
#[test]
fn a_cost_difference_is_withheld_unless_both_arms_priced_every_run() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "with_skill", "completed", None),
            sample_run("run-003", "without_skill", "completed", None),
        ]),
        None,
    );
    for (run_id, timing) in [
        (
            "run-001",
            r#"{ "duration_ms": 1000, "cost": { "kind": "priced", "usd": 0.25 } }"#,
        ),
        ("run-002", r#"{ "duration_ms": 1000 }"#),
        (
            "run-003",
            r#"{ "duration_ms": 1000, "cost": { "kind": "priced", "usd": 0.10 } }"#,
        ),
    ] {
        write_run_artifacts(temp.path(), run_id, None, Some(timing));
    }

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

    assert_eq!(
        benchmark.scenarios["with_skill"].completed.tokens.cost,
        Some(BucketCost::Partial { usd: 0.25, runs: 1 })
    );
    assert_eq!(
        benchmark.scenarios["without_skill"].completed.tokens.cost,
        Some(BucketCost::Whole { usd: 0.10 })
    );
    assert_eq!(
        benchmark
            .deltas
            .with_skill_vs_without_skill
            .expect("both arms ran")
            .cost_usd,
        None
    );
}

/// A run of a harness that prices nothing is not a run that cost nothing, so its
/// bucket names the harness instead of publishing what the other runs came to.
#[test]
fn a_bucket_of_a_harness_that_prices_nothing_names_it_instead_of_totalling() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        None,
        Some(r#"{ "duration_ms": 1000, "cost": { "kind": "unpriced", "harness": "codex" } }"#),
    );

    let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

    assert_eq!(
        benchmark.scenarios["with_skill"].completed.tokens.cost,
        Some(BucketCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}

#[test]
fn benchmark_json_snapshot_matches_fixture_layout() {
    let temp = tempfile::tempdir().unwrap();
    write_report(
        temp.path(),
        serde_json::json!([
            sample_run("run-001", "with_skill", "completed", None),
            sample_run("run-002", "without_skill", "completed", None),
            sample_run("run-003", "old_skill", "completed", None),
        ]),
        None,
    );
    write_run_artifacts(
        temp.path(),
        "run-001",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" },
    { "assertion": "c", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 3, "failed": 0, "total": 3, "pass_rate": 1.0 }
}"#,
        ),
        Some(
            r#"{ "duration_ms": 1000, "total_tokens": 300, "input_tokens": 200, "output_tokens": 100, "cost_usd": 0.01 }"#,
        ),
    );
    write_run_artifacts(
        temp.path(),
        "run-002",
        Some(
            r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": false, "evidence": "nope" }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
        ),
        Some(r#"{ "duration_ms": 2000, "total_tokens": 500, "input_tokens": 300, "output_tokens": 200 }"#),
    );
    write_run_artifacts(
        temp.path(),
        "run-003",
        Some(
            r#"{
  "assertion_results": [{ "assertion": "a", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
        ),
        Some(r#"{ "duration_ms": 1500, "total_tokens": 400 }"#),
    );

    let mut benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    benchmark.generated_at = "2026-05-26T12:00:00Z".to_string();

    let actual = serde_json::to_value(&benchmark).unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/benchmark_expected.json")).unwrap();
    assert_eq!(actual, expected);
}

fn write_drift_report(report_dir: &Path, iteration: u32, evals_hash: &str, eval_ids: &[&str]) {
    fs::create_dir_all(report_dir).unwrap();
    let eval_cases: Vec<serde_json::Value> = eval_ids.iter().map(|id| serde_json::json!({ "id": id })).collect();
    let report = serde_json::json!({
        "report": {
            "id": format!("report-iter-{iteration}"),
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": iteration,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": evals_hash
        },
        "dimensions": {
            "eval_cases": eval_cases,
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": [],
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn eval_suite_drift_warnings_empty_without_a_previous_report() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let report_dir = skill_root.join("report-iter-2");
    write_drift_report(&report_dir, 2, "sha256:current", &["case-a"]);

    let warnings = collect_eval_suite_drift_warnings(&report_dir, BenchmarkOptions::default()).unwrap();

    assert!(warnings.is_empty());
}

#[test]
fn eval_suite_drift_warnings_detected_from_sibling_previous_report() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let report_dir = skill_root.join("report-iter-2");
    write_drift_report(&previous_dir, 1, "sha256:previous", &["case-a", "case-b"]);
    write_drift_report(&report_dir, 2, "sha256:current", &["case-a", "case-c"]);

    let warnings = collect_eval_suite_drift_warnings(&report_dir, BenchmarkOptions::default()).unwrap();

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].added_eval_ids, vec!["case-c".to_string()]);
    assert_eq!(warnings[0].removed_eval_ids, vec!["case-b".to_string()]);
}
