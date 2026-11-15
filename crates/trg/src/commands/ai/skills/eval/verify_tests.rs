use super::*;

/// A validator that cannot say it is not validating is worse than none: strict mode
/// would exit clean on a bundle nothing had read, and the operator would take that
/// for conformance.
#[test]
fn strict_refuses_a_build_that_compiled_the_validator_out() {
    let refusal = VerifyMode::Strict.refusal(SchemaValidation::Absent);
    assert!(refusal.is_some());
    assert!(refusal.unwrap().contains("schema-validation"));
}

#[test]
fn strict_runs_on_a_build_that_can_validate() {
    assert!(VerifyMode::Strict.refusal(SchemaValidation::Compiled).is_none());
}

/// Lenient mode never claimed to check a schema, so a build without the validator
/// takes nothing away from it and it keeps running.
#[test]
fn lenient_is_unaffected_by_a_build_that_cannot_validate() {
    assert!(VerifyMode::Lenient.refusal(SchemaValidation::Absent).is_none());
    assert!(VerifyMode::Lenient.refusal(SchemaValidation::Compiled).is_none());
}

#[test]
fn a_build_with_the_feature_on_reports_that_it_can_validate() {
    assert!(SchemaValidation::of_this_build().is_compiled());
}

use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;

fn verify_args(workspace: Option<PathBuf>, skill_dir: Option<PathBuf>) -> VerifyArgs {
    VerifyArgs {
        workspace_dir: workspace,
        skill_dir,
        eval_dir: None,
        mode: VerifyMode::Lenient,
        require_graders: false,
        output_format: OutputFormat::Json,
        ci: EvalCiArgs::default(),
    }
}

#[test]
fn verifying_a_workspace_records_each_phase_once_under_the_caller() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("run-001").join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut args = verify_args(Some(workspace), None);
    args.ci.fail_on_missing_grading = true;

    let (status, trace) =
        crate::telemetry::testing::capture(|| tracing::info_span!("trg").in_scope(|| args.handle(&crate::fs::RealFS)));

    assert_eq!(status, ExitCode::GateFailed);
    let root = trace.span("trg").unwrap();
    let mut phases: Vec<&str> = trace.children_of(root).iter().map(|span| span.name.as_ref()).collect();
    phases.sort_unstable();
    assert_eq!(
        phases,
        vec![
            "check workspace",
            "collect results",
            "emit annotations",
            "run ci checks"
        ],
        "every collection shares one span"
    );
    assert_eq!(
        trace.attribute("run ci checks", ERROR_TYPE),
        Some(opentelemetry::Value::from(GATE_FAILED))
    );
    assert_eq!(trace.attribute("check workspace", ERROR_TYPE), None);
}

#[test]
fn an_invalid_skill_fails_its_validation_under_the_skill_check() {
    let temp = tempfile::tempdir().unwrap();

    let (status, trace) = crate::telemetry::testing::capture(|| {
        verify_args(None, Some(temp.path().to_path_buf())).handle(&crate::fs::RealFS)
    });

    assert_eq!(status, ExitCode::GateFailed);
    trace.assert_child_of("validate skill", "check skill");
    let invalid = Some(opentelemetry::Value::from(INVALID_SKILL));
    assert_eq!(trace.attribute("validate skill", ERROR_TYPE), invalid);
    assert_eq!(trace.attribute("check skill", ERROR_TYPE), invalid);
    assert!(
        trace.span("check eval suite").is_none(),
        "nothing runs after a skill fails validation"
    );
}
