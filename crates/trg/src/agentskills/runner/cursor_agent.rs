use std::process::Command;
use std::time::Instant;

use super::{
    prepare_workspace, write_timing_file, write_transcript, EvalRunOutcome, EvalRunRequest, RunStatus, RunnerError,
};

const PROGRAM: &str = "cursor-agent";

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request)?;

    let mut command = Command::new(PROGRAM);
    command
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--force")
        .arg("--workspace")
        .arg(request.workspace_dir);

    if let Some(model) = request.runner_model {
        command.arg("--model").arg(model);
    }

    command.arg(&prepared.prompt);

    let start = Instant::now();
    let output = command.output().map_err(|source| RunnerError::Spawn {
        program: PROGRAM.to_string(),
        source,
    })?;
    let wall_ms = start.elapsed().as_millis() as u64;

    write_transcript(request.transcript_path, &output.stdout)?;

    let outcome = parse_outcome(&output.stdout, wall_ms, output.status.success())?;
    write_timing_file(
        &request
            .transcript_path
            .parent()
            .unwrap_or(request.workspace_dir)
            .join("timing.json"),
        &outcome,
    )?;
    Ok(outcome)
}

fn parse_outcome(stdout: &[u8], wall_ms: u64, exit_ok: bool) -> Result<EvalRunOutcome, RunnerError> {
    let text = std::str::from_utf8(stdout).map_err(|_| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail: "stdout was not valid UTF-8".to_string(),
    })?;

    let mut last_result: Option<serde_json::Value> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("result") {
            last_result = Some(value);
        }
    }

    let result = last_result.ok_or_else(|| RunnerError::MissingResult {
        program: PROGRAM.to_string(),
    })?;

    let is_error = !exit_ok || result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let final_text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration_ms = result.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(wall_ms);

    let usage = result.get("usage");
    let input_tokens = usage.and_then(|u| u.get("inputTokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("outputTokens")).and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };

    Ok(EvalRunOutcome {
        status: if is_error {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        },
        duration_ms,
        total_tokens,
        input_tokens,
        output_tokens,
        cost_usd: None,
        final_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terminal_result_event() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":1234,"duration_api_ms":1100,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":0}}
"#;
        let outcome = parse_outcome(stdout, 9999, true).unwrap();
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 1234);
        assert_eq!(outcome.input_tokens, Some(100));
        assert_eq!(outcome.output_tokens, Some(50));
        assert_eq!(outcome.total_tokens, Some(150));
        assert_eq!(outcome.final_text, "hello");
    }

    #[test]
    fn marks_failed_when_is_error_true() {
        let stdout = br#"{"type":"result","is_error":true,"duration_ms":10,"result":"boom"}
"#;
        let outcome = parse_outcome(stdout, 0, true).unwrap();
        assert!(matches!(outcome.status, RunStatus::Failed));
    }

    #[test]
    fn non_zero_exit_overrides_payload_success() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":10,"result":"ok"}
"#;
        let outcome = parse_outcome(stdout, 0, false).unwrap();
        assert!(matches!(outcome.status, RunStatus::Failed));
    }

    #[test]
    fn missing_result_event_errors() {
        let stdout = br#"{"type":"system"}
"#;
        let err = parse_outcome(stdout, 0, true).unwrap_err();
        assert!(matches!(err, RunnerError::MissingResult { .. }));
    }
}
