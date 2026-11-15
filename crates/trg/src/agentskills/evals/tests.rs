use super::*;
use crate::agentskills::budget::RunCost;
use crate::agentskills::report::CacheTokens;
use crate::agentskills::runner::usage::HarnessTokenUsage;
use crate::agentskills::runner::{write_timing_file, EvalRunOutcome, RunStatus, Runner};
use crate::fs::testutil::MemFS;
use std::fs;
use tempfile::tempdir;

/// The three ways a check can fail are not three ways the skill can be wrong. Only the
/// validation failure is a finding; the other two mean the check never read what it was
/// pointed at, and reporting them as findings sends someone to a diff nothing measured.
#[test]
fn only_a_validation_failure_is_a_finding_about_what_was_checked() {
    let validation = EvalError::Validation(ValidationErrors::from(ValidationError::for_field(
        "workspace 'x'",
        "must contain at least one grading.json",
    )));
    let io = EvalError::Io(std::io::Error::other("the artifact could not be read"));
    let json = EvalError::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err());

    assert_eq!(validation.reported_as(), ExitCode::GateFailed);
    assert_eq!(io.reported_as(), ExitCode::InfrastructureFailure);
    assert_eq!(json.reported_as(), ExitCode::InfrastructureFailure);
    assert_ne!(io.reported_as(), validation.reported_as());
    assert_ne!(json.reported_as(), validation.reported_as());
}

#[test]
fn check_eval_suite_accepts_valid_manifest() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": 1,
      "prompt": "Analyze evals/files/sales.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/sales.csv"],
      "graders": [{ "type": "file_exists", "path": "summary.md" }]
    }
  ]
}"#,
    );
    fs.insert(
        Path::new("/csv-analyzer/evals/files/sales.csv"),
        "month,revenue\nMay,10",
    );

    let report = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions {
            require_graders: true,
            ..EvalCheckOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.skill_name, "csv-analyzer");
    assert_eq!(report.eval_count, 1);
    assert_eq!(report.file_count, 1);
}

/// The worked example in docs/how-to/write-graders.md is what a reader is told to
/// copy, so nothing else proves it still loads through the parser that `eval run` uses.
#[test]
fn write_graders_doc_worked_example_loads_through_the_real_parser() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "analyze-sales",
      "prompt": "Analyze evals/files/sales.csv and write a summary.",
      "expected_output": "A markdown summary with revenue totals by month.",
      "files": ["evals/files/sales.csv"],
      "graders": [
        { "type": "file_exists", "path": "summary.md" },
        { "type": "contains", "text": "May", "target": { "file": "summary.md" } },
        { "type": "regex", "pattern": "\\$[0-9,]+", "target": { "file": "summary.md" } },
        { "type": "tool_used", "tool": "Read" },
        {
          "type": "llm",
          "criterion": "The summary reads as a coherent narrative rather than a data dump.",
          "target": { "file": "summary.md" }
        }
      ]
    }
  ]
}"#,
    );
    fs.insert(
        Path::new("/csv-analyzer/evals/files/sales.csv"),
        "month,revenue\nMay,10000",
    );

    let report = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions {
            require_graders: true,
            ..EvalCheckOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.eval_count, 1);
    assert_eq!(report.file_count, 1);

    let suite = load_eval_suite(&fs, Path::new("/csv-analyzer"), &EvalDirName::default()).unwrap();
    assert_eq!(suite.evals[0].graders.len(), 5);
}

/// The same doc's fixture-pairing example, where neither check has a typed equivalent,
/// so both are declared as `llm` rather than left as an implicit assertion fallback.
#[test]
fn write_graders_doc_fixture_pairing_example_loads_through_the_real_parser() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "merge-reports",
      "prompt": "Merge evals/files/q1.csv and evals/files/q2.csv into a single report.",
      "expected_output": "Combined quarterly report.",
      "files": [
        "evals/files/q1.csv",
        "evals/files/q2.csv"
      ],
      "graders": [
        { "type": "llm", "criterion": "The workspace contains exactly one combined output file." },
        { "type": "llm", "criterion": "The combined output includes rows from both input files." }
      ]
    }
  ]
}"#,
    );
    fs.insert(Path::new("/csv-analyzer/evals/files/q1.csv"), "month,revenue\nJan,5000");
    fs.insert(Path::new("/csv-analyzer/evals/files/q2.csv"), "month,revenue\nFeb,6000");

    let report = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions {
            require_graders: true,
            ..EvalCheckOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.eval_count, 1);
    assert_eq!(report.file_count, 2);

    let suite = load_eval_suite(&fs, Path::new("/csv-analyzer"), &EvalDirName::default()).unwrap();
    assert_eq!(suite.evals[0].graders.len(), 2);
}

#[test]
fn check_eval_suite_rejects_fixture_exceeding_byte_limit() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "large-file",
      "prompt": "Analyze evals/files/sales.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/sales.csv"]
    }
  ]
}"#,
    );
    fs.insert(
        Path::new("/csv-analyzer/evals/files/sales.csv"),
        "month,revenue\nMay,10",
    );

    let err = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions {
            max_fixture_bytes: Some(10),
            ..EvalCheckOptions::default()
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("exceeds 10 byte limit"));
}

/// Every case's companions are put through the same validation `verify` gives the rest
/// of the suite, so a case that could never stage is reported before a pass spends a run
/// finding out.
#[test]
fn check_eval_suite_rejects_a_companion_that_is_not_a_skill_directory() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "triggers-the-skill",
      "prompt": "Work out what happened to revenue last month",
      "expected_output": "A short summary.",
      "skill_disclosure": "unannounced",
      "companion_skills": ["evals/companions/not-a-skill"],
      "graders": [{ "type": "file_exists", "path": "summary.md" }]
    }
  ]
}"#,
    );
    fs.insert(Path::new("/csv-analyzer/evals/companions/not-a-skill/README.md"), "hi");

    let err = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("evals/companions/not-a-skill"), "{err}");
}

#[test]
fn check_eval_suite_accepts_a_companion_that_is_a_skill_directory() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "triggers-the-skill",
      "prompt": "Work out what happened to revenue last month",
      "expected_output": "A short summary.",
      "skill_disclosure": "unannounced",
      "companion_skills": ["evals/companions/log-summarizer"],
      "graders": [{ "type": "file_exists", "path": "summary.md" }]
    }
  ]
}"#,
    );
    fs.insert(
        Path::new("/csv-analyzer/evals/companions/log-summarizer/SKILL.md"),
        "---\nname: log-summarizer\ndescription: Summarizes application log files into a short report.\n---\n\nBody.\n",
    );

    let report = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions::default(),
    )
    .expect("a valid companion passes verify");

    assert_eq!(report.eval_count, 1);
}

#[test]
fn check_eval_suite_rejects_missing_fixture_file() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "missing-file",
      "prompt": "Analyze evals/files/missing.csv",
      "expected_output": "A short summary.",
      "files": ["evals/files/missing.csv"]
    }
  ]
}"#,
    );

    let err = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn check_eval_suite_rejects_paths_outside_skill_directory() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "outside",
      "prompt": "Analyze a fixture",
      "expected_output": "A short summary.",
      "files": ["../outside.csv"]
    }
  ]
}"#,
    );

    let err = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("must stay inside the skill directory"));
}

#[test]
fn glob_pattern_accepts_the_supported_wildcards() {
    let pattern: GlobPattern = serde_json::from_value(serde_json::json!("**/*.md")).unwrap();
    assert_eq!(pattern.as_str(), "**/*.md");
    assert!(!pattern.is_literal());
}

#[test]
fn glob_pattern_without_wildcards_is_literal() {
    let pattern: GlobPattern = serde_json::from_value(serde_json::json!("report.md")).unwrap();
    assert!(pattern.is_literal());
    assert_eq!(pattern.as_literal_path(), Some(Path::new("report.md")));
}

#[test]
fn glob_pattern_rejects_character_classes() {
    let err = GlobPattern::parse("file[0-9].md").unwrap_err();
    assert!(err.contains("only *, **, and ? are supported"), "{err}");
}

#[test]
fn glob_pattern_rejects_a_path_that_escapes_the_skill_directory() {
    let err = GlobPattern::parse("../outside.md").unwrap_err();
    assert!(err.contains("must stay inside the skill directory"), "{err}");
}

#[test]
fn glob_pattern_compiles_a_star_to_stay_within_one_segment() {
    let pattern = GlobPattern::parse("*.md").unwrap();
    let regex = pattern.compile();
    assert!(regex.is_match("report.md"));
    assert!(!regex.is_match("nested/report.md"));
}

#[test]
fn glob_pattern_compiles_a_double_star_to_cross_segments() {
    let pattern = GlobPattern::parse("**/*.md").unwrap();
    let regex = pattern.compile();
    assert!(regex.is_match("report.md"));
    assert!(regex.is_match("nested/deeper/report.md"));
    assert!(!regex.is_match("report.txt"));
}

#[test]
fn glob_pattern_compiles_a_question_mark_to_one_character() {
    let pattern = GlobPattern::parse("log?.txt").unwrap();
    let regex = pattern.compile();
    assert!(regex.is_match("log1.txt"));
    assert!(!regex.is_match("log12.txt"));
}

#[test]
fn check_eval_suite_requires_graders_when_requested() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/csv-analyzer/evals/evals.json"),
        r#"{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "no-assertions",
      "prompt": "Analyze input",
      "expected_output": "A short summary."
    }
  ]
}"#,
    );

    let err = check_eval_suite(
        &fs,
        Path::new("/csv-analyzer"),
        &EvalDirName::default(),
        "csv-analyzer",
        EvalCheckOptions {
            require_graders: true,
            ..EvalCheckOptions::default()
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("must define at least one grader"));
}

#[test]
fn verifying_a_workspace_path_sees_the_grading_written_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("run-001");
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        run_dir.join("grading.json"),
        r#"{
  "assertion_results": [
    {
      "assertion": "file 'summary.md' exists",
      "passed": true,
      "evidence": "'summary.md' exists and holds 20 bytes",
      "grader": { "kind": "declarative" }
    }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 1, "unsupported": 0, "pass_rate": 1.0 }
}"#,
    )
    .unwrap();

    let report = check_workspace(
        &workspace,
        WorkspaceCheckOptions {
            require_grading: true,
            fail_on_failed_assertions: true,
        },
    )
    .unwrap();

    assert_eq!(report.grading_files, 1);
    assert_eq!(report.passed_assertions, 1);
}

#[test]
fn a_directory_that_is_not_a_workspace_is_searched_as_given() {
    let tmp = tempfile::tempdir().unwrap();
    let sibling = tmp.path().join("sibling");
    let target = tmp.path().join("target");
    fs::create_dir_all(&sibling).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(sibling.join("grading.json"), "{}").unwrap();

    let root = ArtifactSearchRoot::for_workspace(&target);

    assert_eq!(root.as_path(), target.as_path());
}

#[test]
fn check_workspace_aggregates_grading_and_timing_files() {
    let tmp = tempdir().unwrap();
    let run = tmp.path().join("iteration-1").join("eval-one").join("with_skill");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("grading.json"),
        r#"{
  "assertion_results": [
    {
      "assertion": "Includes a summary",
      "passed": true,
      "evidence": "summary.md exists in outputs",
      "grader": { "kind": "mechanical" }
    },
    {
      "assertion": "Includes a chart",
      "passed": false,
      "evidence": "no chart file found in outputs",
      "grader": { "kind": "mechanical" }
    }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
    )
    .unwrap();
    fs::write(
        run.join("timing.json"),
        r#"{ "total_tokens": 1000, "duration_ms": 2500 }"#,
    )
    .unwrap();

    let report = check_workspace(tmp.path(), WorkspaceCheckOptions::default()).unwrap();

    assert_eq!(report.grading_files, 1);
    assert_eq!(report.timing_files, 1);
    assert_eq!(report.assertion_results, 2);
    assert_eq!(report.passed_assertions, 1);
    assert_eq!(report.failed_assertions, 1);
    assert_eq!(report.pass_rate, Some(0.5));
}

#[test]
fn read_timing_file_accepts_runner_written_timing_file() {
    let tmp = tempdir().unwrap();
    let timing_path = tmp.path().join("run").join("timing.json");
    let outcome = EvalRunOutcome {
        status: RunStatus::Completed,
        failure_kind: None,
        duration_ms: 2500,
        exit_code: Some(0),
        tokens: HarnessTokenUsage::reported(Some(700), Some(300), Some(CacheTokens::parse(Some(50), None).unwrap())),
        cost: Some(RunCost::Priced { usd: 0.42 }),
        final_text: "done".to_string(),
        read_only_fixture_violations: Vec::new(),
    };

    write_timing_file(&timing_path, &outcome).unwrap();
    let timing = read_timing_file(&timing_path).unwrap();

    assert_eq!(timing.duration_ms, 2500);
    assert_eq!(timing.exit_code, Some(0));
    assert_eq!(timing.total_tokens, Some(1000));
    assert_eq!(timing.input_tokens, Some(700));
    assert_eq!(timing.output_tokens, Some(300));
    assert_eq!(timing.cached_tokens, Some(CacheTokens::parse(Some(50), None).unwrap()));
    assert_eq!(timing.cost, Some(RunCost::Priced { usd: 0.42 }));
}

/// A harness that prices none of its runs and a harness that priced every run but this
/// one are two different facts about a bill. An absent number carries neither, and the
/// reader totalling a mixed history adds it as a zero.
#[test]
fn a_timing_file_for_a_harness_that_prices_nothing_names_the_harness() {
    let tmp = tempdir().unwrap();
    let timing_path = tmp.path().join("run").join("timing.json");
    let outcome = EvalRunOutcome {
        cost: Runner::Codex.pricing().price(None),
        ..priced_outcome()
    };

    write_timing_file(&timing_path, &outcome).unwrap();

    let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&timing_path).unwrap()).unwrap();
    assert_eq!(written["cost"]["kind"], "unpriced");
    assert_eq!(written["cost"]["harness"], "codex");
    assert!(
        written.get("cost_usd").is_none(),
        "a run nobody priced has no number to publish under the field that only ever held one"
    );
    assert_eq!(
        read_timing_file(&timing_path).unwrap().cost,
        Some(RunCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}

/// The field an earlier release wrote is the field its readers still look at, so a run
/// that carries a price keeps publishing one under that name.
#[test]
fn a_timing_file_for_a_priced_run_still_publishes_the_bare_number() {
    let tmp = tempdir().unwrap();
    let timing_path = tmp.path().join("run").join("timing.json");

    write_timing_file(&timing_path, &priced_outcome()).unwrap();

    let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&timing_path).unwrap()).unwrap();
    assert_eq!(written["cost_usd"], 0.42);
    assert_eq!(written["cost"]["kind"], "priced");
}

/// Only a harness that prices its runs ever wrote this number, so a file carrying one
/// is a priced run however old the release that wrote it.
#[test]
fn a_timing_file_written_before_a_run_could_say_why_it_has_no_price_reads_as_priced() {
    let tmp = tempdir().unwrap();
    let timing_path = tmp.path().join("timing.json");
    fs::write(&timing_path, r#"{ "duration_ms": 2500, "cost_usd": 0.42 }"#).unwrap();

    let timing = read_timing_file(&timing_path).unwrap();

    assert_eq!(timing.cost, Some(RunCost::Priced { usd: 0.42 }));
}

fn priced_outcome() -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Completed,
        failure_kind: None,
        duration_ms: 2500,
        exit_code: Some(0),
        tokens: HarnessTokenUsage::reported(Some(700), Some(300), None),
        cost: Runner::ClaudeCode.pricing().price(Some(0.42)),
        final_text: "done".to_string(),
        read_only_fixture_violations: Vec::new(),
    }
}

#[test]
fn read_timing_file_rejects_unknown_fields() {
    let tmp = tempdir().unwrap();
    let timing_path = tmp.path().join("timing.json");
    fs::write(&timing_path, r#"{ "duration_ms": 2500, "wall_clock_ms": 2500 }"#).unwrap();

    let error = read_timing_file(&timing_path).unwrap_err();

    assert!(error.to_string().contains("wall_clock_ms"));
}

#[test]
fn check_workspace_can_fail_on_failed_assertions() {
    let tmp = tempdir().unwrap();
    let run = tmp.path().join("iteration-1").join("eval-one").join("with_skill");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("grading.json"),
        r#"{
  "assertion_results": [
    {
      "assertion": "Includes a summary",
      "passed": false,
      "evidence": "summary.md missing from outputs",
      "grader": { "kind": "mechanical" }
    }
  ],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
    )
    .unwrap();

    let err = check_workspace(
        tmp.path(),
        WorkspaceCheckOptions {
            require_grading: true,
            fail_on_failed_assertions: true,
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed assertion"));
}

fn sample_suite_with_eval(eval: EvalCase) -> EvalSuite {
    EvalSuite {
        skill_name: NonEmptyString("demo-skill".to_string()),
        eval_dir: None,
        evals: vec![eval],
    }
}

fn sample_eval_case(id: &str, prompt: &str, expected_output: &str) -> EvalCase {
    EvalCase {
        attempts: None,
        model: None,
        env: None,
        append_system_prompt: None,
        id: EvalCaseId(id.to_string()),
        name: None,
        description: None,
        prompt: NonEmptyString(prompt.to_string()),
        expected_output: NonEmptyString(expected_output.to_string()),
        files: vec![],
        tags: None,
        priority: None,
        split: EvalSplit::default(),
        timeout_secs: None,
        expected_output_files: None,
        grader_hints: None,
        graders: vec![serde_json::from_value(serde_json::json!({
            "type": "file_exists",
            "path": "summary.md"
        }))
        .unwrap()],
        skill_disclosure: SkillDisclosure::default(),
        scaffold: None,
        companion_skills: Vec::new(),
        conversation_history: None,
        allowed_tools: None,
    }
}

#[test]
fn lint_eval_suite_warns_on_vague_prompt() {
    let suite = sample_suite_with_eval(sample_eval_case("one", "too short", "long enough output"));

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings.iter().any(|warning| warning.message.contains("too vague")));
}

#[test]
fn lint_eval_suite_warns_on_generic_expected_output() {
    let suite = sample_suite_with_eval(sample_eval_case("one", "A sufficiently long prompt here", "short"));

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings.iter().any(|warning| warning.message.contains("too generic")));
}

#[test]
fn lint_eval_suite_warns_when_fixtures_are_not_referenced() {
    let mut eval = sample_eval_case(
        "one",
        "Analyze the attached data without paths",
        "A detailed analysis output",
    );
    eval.files = vec![EvalFixture::writable(RelativeSkillPath(
        "evals/files/input.csv".to_string(),
    ))];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings
        .iter()
        .any(|warning| warning.message.contains("does not reference a file path")));
}

#[test]
fn lint_eval_suite_warns_on_duplicate_fixture_paths() {
    let mut eval = sample_eval_case(
        "one",
        "Analyze evals/files/input.csv twice",
        "A detailed analysis output",
    );
    eval.files = vec![
        EvalFixture::writable(RelativeSkillPath("evals/files/input.csv".to_string())),
        EvalFixture::writable(RelativeSkillPath("evals/files/input.csv".to_string())),
    ];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings
        .iter()
        .any(|warning| warning.message.contains("duplicate fixture path")));
}

#[test]
fn lint_eval_suite_warns_on_empty_assertions_by_default() {
    let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
    eval.graders = vec![];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings
        .iter()
        .any(|warning| warning.message.contains("no graders are declared")));
}

#[test]
fn lint_eval_suite_does_not_warn_when_typed_graders_are_declared() {
    let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
    eval.split = EvalSplit::Test;
    let suite = sample_suite_with_eval(eval);

    assert!(lint_eval_suite(&suite, EvalLintOptions::default()).is_empty());
}

#[test]
fn lint_eval_suite_warns_when_no_case_declares_the_test_split() {
    let eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings
        .iter()
        .any(|warning| warning.message.contains("no case declares")));
}

#[test]
fn lint_eval_suite_allows_empty_assertions_when_requested() {
    let mut eval = sample_eval_case("one", "A sufficiently long prompt here", "A detailed analysis output");
    eval.graders = vec![];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(
        &suite,
        EvalLintOptions {
            allow_empty_graders: true,
            ..EvalLintOptions::default()
        },
    );
    assert!(!warnings
        .iter()
        .any(|warning| warning.message.contains("no graders are declared")));
}

/// The contradiction this lint exists to catch: a case that checks whether the
/// run reaches for the skill, in a prompt that hands it the skill.
#[test]
fn a_case_that_checks_triggering_while_announcing_the_skill_is_linted() {
    let eval: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "triggers",
        "prompt": "Turn the staged sales file into a summary for the finance team.",
        "expected_output": "The run reaches for the skill rather than improvising.",
        "graders": [{ "type": "skill_used" }],
    }))
    .unwrap();
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(warnings
        .iter()
        .any(|warning| warning.message.contains("skill_disclosure")));
}

#[test]
fn the_scaffolds_triggering_case_can_measure_triggering() {
    let suite = scaffold_eval_suite("demo-skill");
    let triggering = suite
        .evals
        .iter()
        .find(|eval| eval.id.as_str() == "triggers-the-skill")
        .expect("the scaffold declares a triggering case");

    assert_eq!(triggering.skill_disclosure, SkillDisclosure::Unannounced);
    assert!(triggering.checks_skill_engagement());
    assert!(!triggering.prompt.as_str().contains("demo-skill"));

    let warnings = lint_eval_suite(&suite, EvalLintOptions::default());
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.message.contains("skill_disclosure")),
        "the shipped scaffold must not trip its own lint: {warnings:?}"
    );
}

#[test]
fn scaffold_eval_suite_round_trips_through_serde_json() {
    let suite = scaffold_eval_suite("demo-skill");
    let json = serde_json::to_string_pretty(&suite).unwrap();
    let parsed = parse_eval_suite(&json).unwrap();
    assert_eq!(parsed.skill_name.as_str(), "demo-skill");
    assert_eq!(parsed.evals.len(), 2);
    assert_eq!(parsed.evals[0].id.as_str(), "produces-a-summary");
    assert_eq!(parsed.evals[1].id.as_str(), "triggers-the-skill");
    assert!(parsed.evals.iter().all(EvalCase::has_checks));
}

#[test]
fn scaffold_declares_only_typed_graders() {
    let suite = scaffold_eval_suite("demo-skill");
    assert!(suite.evals.iter().all(|eval| !eval.graders.is_empty()));
}

#[test]
fn scaffold_eval_suite_passes_strict_check() {
    let fs = MemFS::new();
    fs.insert(
        Path::new("/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\n",
    );
    write_eval_suite(
        &fs,
        Path::new("/demo-skill"),
        &EvalDirName::default(),
        &scaffold_eval_suite("demo-skill"),
    )
    .unwrap();

    check_eval_suite(
        &fs,
        Path::new("/demo-skill"),
        &EvalDirName::default(),
        "demo-skill",
        EvalCheckOptions {
            require_graders: true,
            ..EvalCheckOptions::default()
        },
    )
    .unwrap();
}

#[test]
fn parse_v2_manifest_with_all_metadata_fields_round_trips() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "full",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "files": ["evals/files/input.csv"],
      "tags": ["smoke", "regression"],
      "priority": "critical",
      "timeout_secs": 90,
      "expected_output_files": ["report.md", "summary.md"],
      "grader_hints": { "strict_json": true, "threshold": 0.8 }
    }
  ]
}"#;

    let suite = parse_eval_suite(json).unwrap();
    let round_trip = serde_json::to_string(&suite).unwrap();
    let reparsed = parse_eval_suite(&round_trip).unwrap();
    let eval = &reparsed.evals[0];
    assert_eq!(
        eval.tags.as_deref(),
        Some(["smoke".to_string(), "regression".to_string()].as_slice())
    );
    assert_eq!(eval.priority, Some(EvalPriority::Critical));
    assert_eq!(eval.timeout_secs, Some(90));
    assert_eq!(
        eval.expected_output_files.as_deref(),
        Some(["report.md".to_string(), "summary.md".to_string()].as_slice())
    );
    assert_eq!(
        eval.grader_hints
            .as_ref()
            .and_then(|hints| hints.get("strict_json"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[test]
fn a_manifest_without_schema_version_parses_case_metadata_fields() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "tags": ["smoke"]
    }
  ]
}"#;

    let suite = parse_eval_suite(json).expect("tags must parse without a declared schema_version");
    assert_eq!(suite.evals[0].tags.as_deref(), Some(&["smoke".to_string()][..]));
}

#[test]
fn parse_v2_manifest_rejects_unknown_suite_fields() {
    let json = r#"{
  "skill_name": "demo-skill",
  "future_suite_field": true,
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err();
    assert!(err.to_string().contains("future_suite_field"));
}

#[test]
fn parse_rejects_a_misspelled_case_field_regardless_of_schema_version() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "assertion": [
        "The response follows the skill instructions"
      ]
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err().to_string();
    assert!(err.contains("unknown field `assertion`"), "{err}");
    assert!(err.contains("expected one of"), "{err}");
}

/// An announced prompt names the skill and where it sits, so it settled the routing
/// decision before the run began. A companion there would stage a skill the prompt has
/// already ruled out, and a case that says nothing about that reads as measuring a
/// choice nothing was ever going to make.
#[test]
fn a_companion_declared_on_an_announced_case_is_refused_by_name() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "companion_skills": ["evals/companions/other-skill"]
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err().to_string();

    assert!(err.contains("evals/companions/other-skill"), "{err}");
    assert!(err.contains("skill_disclosure"), "{err}");
}

#[test]
fn a_companion_declared_on_an_unannounced_case_parses() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "skill_disclosure": "unannounced",
      "companion_skills": ["evals/companions/other-skill"]
    }
  ]
}"#;

    let suite = parse_eval_suite(json).expect("an unannounced case may declare companions");

    assert_eq!(suite.evals[0].companion_skills.len(), 1);
    assert_eq!(
        suite.evals[0].companion_skills[0].as_str(),
        "evals/companions/other-skill"
    );
}

/// A companion is a skill directory this run executes against, and the trust gate is
/// answered per directory. Keeping a companion inside the skill directory is what makes
/// the answer already given for the skill cover it too.
#[test]
fn a_companion_that_names_a_directory_outside_the_skill_is_refused() {
    for escape in ["/etc/passwd", "../elsewhere"] {
        let json = format!(
            r#"{{
  "skill_name": "demo-skill",
  "evals": [
    {{
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "skill_disclosure": "unannounced",
      "companion_skills": [{}]
    }}
  ]
}}"#,
            serde_json::to_string(escape).unwrap()
        );

        let err = parse_eval_suite(&json).unwrap_err().to_string();

        assert!(err.contains("companion skill"), "{escape}: {err}");
    }
}

#[test]
fn parse_current_version_manifest_rejects_unknown_suite_fields() {
    let json = r#"{
  "skill_name": "demo-skill",
  "future_suite_field": true,
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err().to_string();
    assert!(err.contains("future_suite_field"), "{err}");
}

#[test]
fn parse_current_version_manifest_rejects_misspelled_case_fields() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    },
    {
      "id": "two",
      "prompt": "Another sufficiently long prompt",
      "expected_output": "Another detailed analysis output",
      "grader": [
        { "type": "skill_used" }
      ]
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err().to_string();
    assert!(err.contains("grader"), "{err}");
    assert!(err.contains("evals[1]"), "{err}");
}

/// A case whose stability is the question needs more draws than the pass default, and a
/// case that is expensive needs fewer, without either one dictating the whole suite.
#[test]
fn a_case_can_pin_its_own_draw_count() {
    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");

    eval.attempts = Some(AttemptCount::parse(7).unwrap());
    assert_eq!(effective_attempts(&eval, None).count(), 7);

    eval.attempts = None;
    assert_eq!(effective_attempts(&eval, None), AttemptCount::recommended());
}

/// An operator forcing a cheap smoke pass or a deep one has to be able to override every
/// case, which is why the CLI count is optional rather than defaulted: a defaulted count
/// is indistinguishable from one they typed.
#[test]
fn an_operator_who_names_a_count_overrides_every_case_that_pinned_one() {
    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
    eval.attempts = Some(AttemptCount::parse(7).unwrap());

    assert_eq!(effective_attempts(&eval, Some(AttemptCount::single())).count(), 1);
}

#[test]
fn a_suite_cannot_pin_a_case_to_zero_draws() {
    let refused = serde_json::from_value::<EvalSuite>(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "attempts": 0
        }]
    }));

    assert!(refused.is_err());
}

/// A case that asks about one model in particular has to get that model even when the
/// operator named another at the CLI, or the case is silently answering a different
/// question than the one it states.
#[test]
fn a_case_that_names_a_model_gets_it_over_the_one_the_operator_asked_for() {
    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
    let operator = ModelName::parse("operator-model").unwrap();

    eval.model = Some(ModelName::parse("case-model").unwrap());
    assert_eq!(effective_model(&eval, Some(&operator)).unwrap().as_str(), "case-model");

    eval.model = None;
    assert_eq!(
        effective_model(&eval, Some(&operator)).unwrap().as_str(),
        "operator-model"
    );
    assert_eq!(effective_model(&eval, None), None);
}

/// A reader of `runner_model_source` has to be able to tell a case's own pin apart from
/// the operator's flag apart from a harness default that no one named, since two runs
/// can carry the same model name for different reasons.
#[test]
fn effective_model_source_names_who_actually_decided() {
    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
    let operator = ModelName::parse("operator-model").unwrap();

    eval.model = Some(ModelName::parse("case-model").unwrap());
    assert_eq!(effective_model_source(&eval, Some(&operator)), ModelSource::Case);
    assert_eq!(effective_model_source(&eval, None), ModelSource::Case);

    eval.model = None;
    assert_eq!(effective_model_source(&eval, Some(&operator)), ModelSource::RunnerFlag);
    assert_eq!(effective_model_source(&eval, None), ModelSource::RunnerDefault);
}

/// The manifest is `deny_unknown_fields`, so a suite naming a model only runs if the
/// field is spelled the way a suite author would spell it.
#[test]
fn a_suite_can_name_a_models_per_case() {
    let suite: EvalSuite = serde_json::from_value(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "model": "claude-opus-5"
        }]
    }))
    .unwrap();

    assert_eq!(suite.evals[0].model.as_ref().unwrap().as_str(), "claude-opus-5");
}

/// A blank model is not a weaker request, it is one a harness would answer with its own
/// default while the suite reads as having pinned something.
#[test]
fn a_suite_cannot_name_a_blank_model() {
    let refused = serde_json::from_value::<EvalSuite>(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "model": "   "
        }]
    }));

    assert!(refused.is_err());
}

#[test]
fn a_suite_can_give_a_case_its_own_variables() {
    let suite: EvalSuite = serde_json::from_value(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "env": { "EVAL_REGION": "eu-west-1" }
        }]
    }))
    .unwrap();

    assert_eq!(
        suite.evals[0].env.as_ref().unwrap().iter().collect::<Vec<_>>(),
        vec![("EVAL_REGION", "eu-west-1")]
    );
}

/// Refused while the suite is read rather than when the run is assembled: by the time a
/// harness has been handed the rewritten environment, the policy the operator chose has
/// already been edited out from under them.
#[test]
fn a_suite_cannot_set_a_variable_the_isolation_policy_owns() {
    let refused = serde_json::from_value::<EvalSuite>(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "env": { "PATH": "/evil/bin" }
        }]
    }));

    assert!(refused.is_err());
}

#[test]
fn a_suite_can_steer_a_case_with_its_own_system_prompt_appendix() {
    let suite: EvalSuite = serde_json::from_value(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "append_system_prompt": "Answer in British English."
        }]
    }))
    .unwrap();

    assert_eq!(
        suite.evals[0].append_system_prompt.as_ref().unwrap().as_str(),
        "Answer in British English."
    );
}

/// A blank appendix asks for the case to be steered and then steers it nowhere, so the
/// run would be recorded as carrying an instruction it never carried.
#[test]
fn a_suite_cannot_steer_a_case_with_a_blank_appendix() {
    let refused = serde_json::from_value::<EvalSuite>(serde_json::json!({
        "skill_name": "demo",
        "evals": [{
            "id": "one",
            "prompt": "a prompt long enough to pass",
            "expected_output": "an output long enough",
            "append_system_prompt": "   "
        }]
    }));

    assert!(refused.is_err());
}

#[test]
fn effective_timeout_secs_prefers_per_eval_override() {
    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
    eval.timeout_secs = Some(42);
    assert_eq!(effective_timeout_secs(&eval, Some(99)), Some(42));
    eval.timeout_secs = None;
    assert_eq!(effective_timeout_secs(&eval, Some(99)), Some(99));
    assert_eq!(effective_timeout_secs(&eval, None), None);
}

#[test]
fn lint_eval_suite_warns_on_large_fixture() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    let fixture_path = skill_path.join("evals/files/large.csv");
    std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture_path, vec![b'a'; DEFAULT_MAX_FIXTURE_BYTES as usize + 1]).unwrap();

    let mut eval = sample_eval_case(
        "one",
        "Analyze evals/files/large.csv carefully",
        "A detailed analysis output",
    );
    eval.files = vec![EvalFixture::writable(RelativeSkillPath(
        "evals/files/large.csv".to_string(),
    ))];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
    assert!(warnings.iter().any(|warning| warning.message.contains("exceeds")));
}

#[test]
fn lint_eval_suite_does_not_warn_on_small_fixture() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    let fixture_path = skill_path.join("evals/files/small.csv");
    std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture_path, "month,revenue\nMay,10").unwrap();

    let mut eval = sample_eval_case(
        "one",
        "Analyze evals/files/small.csv carefully",
        "A detailed analysis output",
    );
    eval.files = vec![EvalFixture::writable(RelativeSkillPath(
        "evals/files/small.csv".to_string(),
    ))];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
    assert!(!warnings.iter().any(|warning| warning.message.contains("exceeds")));
}

#[test]
fn lint_eval_suite_warns_on_binary_fixture() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    let fixture_path = skill_path.join("evals/files/binary.bin");
    std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture_path, b"text\0binary").unwrap();

    let mut eval = sample_eval_case(
        "one",
        "Analyze evals/files/binary.bin carefully",
        "A detailed analysis output",
    );
    eval.files = vec![EvalFixture::writable(RelativeSkillPath(
        "evals/files/binary.bin".to_string(),
    ))];
    let suite = sample_suite_with_eval(eval);

    let warnings = lint_eval_suite_fixtures(&crate::fs::RealFS, &skill_path, &suite, EvalLintOptions::default());
    assert!(warnings.iter().any(|warning| warning.message.contains("binary")));
}

#[test]
fn missing_expected_output_warnings_lists_missing_files() {
    let temp = tempdir().unwrap();
    let outputs = temp.path().join("outputs");
    std::fs::create_dir_all(&outputs).unwrap();
    std::fs::write(outputs.join("present.md"), "ok").unwrap();

    let mut eval = sample_eval_case("one", "prompt long enough here", "output long");
    eval.expected_output_files = Some(vec!["present.md".to_string(), "missing.md".to_string()]);

    let warnings = missing_expected_output_warnings(&eval, &outputs);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("missing.md"));
}

#[test]
fn a_case_that_declares_two_graders_with_the_same_name_is_refused() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output",
      "graders": [
        { "type": "skill_used", "name": "primary" },
        { "type": "contains", "text": "total", "name": "primary" }
      ]
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err().to_string();
    assert!(err.contains("duplicate name 'primary'"));
}

#[test]
fn a_case_with_a_name_and_description_round_trips_through_serde_json() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "name": "Readable case title",
      "description": "What this case is checking for.",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

    let suite = parse_eval_suite(json).unwrap();
    let round_tripped: EvalSuite = parse_eval_suite(&serde_json::to_string(&suite).unwrap()).unwrap();

    assert_eq!(
        round_tripped.evals[0].name.as_ref().map(NonEmptyString::to_string),
        Some("Readable case title".to_string())
    );
    assert_eq!(
        round_tripped.evals[0]
            .description
            .as_ref()
            .map(NonEmptyString::to_string),
        Some("What this case is checking for.".to_string())
    );
}

#[test]
fn a_case_with_an_empty_name_is_refused() {
    let json = r#"{
  "skill_name": "demo-skill",
  "evals": [
    {
      "id": "one",
      "name": "",
      "prompt": "A sufficiently long prompt here",
      "expected_output": "A detailed analysis output"
    }
  ]
}"#;

    let err = parse_eval_suite(json).unwrap_err();
    assert!(err.to_string().contains("must be a non-empty string"));
}

#[test]
fn a_case_without_a_name_or_description_omits_them_from_serialized_json() {
    let suite = sample_suite_with_eval(sample_eval_case(
        "one",
        "A sufficiently long prompt here",
        "A detailed analysis output",
    ));

    let json = serde_json::to_string(&suite.evals[0]).unwrap();
    assert!(!json.contains("\"name\""));
    assert!(!json.contains("\"description\""));
}

#[cfg(test)]
mod eval_fixture_tests;
