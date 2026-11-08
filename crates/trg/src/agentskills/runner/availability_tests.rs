use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn write_stub_runner(dir: &Path, name: &str, version_line: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\necho '{version_line}'\n")).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

#[test]
fn check_runner_available_finds_stub_binary() {
    let temp = tempfile::tempdir().unwrap();
    write_stub_runner(temp.path(), "codex", "codex 9.9.9");

    let probe = check_runner_available_on_path(Runner::Codex, temp.path().to_str().unwrap()).unwrap();
    assert!(probe.binary_path.starts_with(temp.path()));
    assert!(probe.binary_path.ends_with("codex"));
}

#[test]
fn check_runner_available_returns_structured_error_for_empty_path() {
    let err = check_runner_available_on_path(Runner::Codex, "").unwrap_err();
    assert_eq!(err.runner, Runner::Codex);
    assert_eq!(err.binary_name, "codex");
    assert!(err.searched_paths.is_empty());
}

#[test]
fn capture_version_reads_first_stdout_line() {
    let temp = tempfile::tempdir().unwrap();
    write_stub_runner(temp.path(), "stub-runner", "stub-runner 1.2.3");
    let version = capture_version(&temp.path().join("stub-runner")).expect("stub --version should succeed");
    assert_eq!(version, "stub-runner 1.2.3");
}

#[test]
fn a_version_probe_is_a_cli_client_span_named_for_the_binary() {
    let temp = tempfile::tempdir().unwrap();
    write_stub_runner(temp.path(), "stub-runner", "stub-runner 1.2.3");
    let (version, trace) = crate::telemetry::testing::capture(|| capture_version(&temp.path().join("stub-runner")));
    assert_eq!(version.as_deref(), Some("stub-runner 1.2.3"));

    let span = trace.span("stub-runner").expect("probe span exported");
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Client);
    assert_eq!(
        trace
            .attribute(
                "stub-runner",
                opentelemetry_semantic_conventions::attribute::PROCESS_EXIT_CODE
            )
            .map(|value| value.to_string()),
        Some("0".to_string())
    );
    assert!(trace
        .attribute(
            "stub-runner",
            opentelemetry_semantic_conventions::attribute::PROCESS_PID
        )
        .is_some());
}
