use std::process::Command;
use std::time::Instant;

use super::{
    prepare_workspace, write_timing_file, write_transcript, EvalRunOutcome, EvalRunRequest, RunStatus, RunnerError,
};

const PROGRAM: &str = "claude";

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request)?;

    let mut command = Command::new(PROGRAM);
    command
        .current_dir(request.workspace_dir)
        .arg("-p")
        .arg(&prepared.prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");

    if let Some(model) = request.model {
        command.arg("--model").arg(model);
    }

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

    let is_error = result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(!exit_ok);
    let final_text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration_ms = result.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(wall_ms);

    let usage = result.get("usage");
    let input_tokens = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64());
    let cache_read = usage
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_u64());
    let cache_creation = usage
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (None, None) => None,
        (i, o) => Some(i.unwrap_or(0) + o.unwrap_or(0) + cache_read.unwrap_or(0) + cache_creation.unwrap_or(0)),
    };
    let cost_usd = result.get("total_cost_usd").and_then(|v| v.as_f64());

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
        cost_usd,
        final_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_with_cost_and_cache_tokens() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","total_cost_usd":0.0123,"usage":{"input_tokens":80,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":0}}
"#;
        let outcome = parse_outcome(stdout, 9999, true).unwrap();
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 2000);
        assert_eq!(outcome.input_tokens, Some(80));
        assert_eq!(outcome.output_tokens, Some(20));
        assert_eq!(outcome.total_tokens, Some(105));
        assert_eq!(outcome.cost_usd, Some(0.0123));
        assert_eq!(outcome.final_text, "final text");
    }

    #[test]
    fn missing_result_event_errors() {
        let stdout = br#"{"type":"system"}
"#;
        let err = parse_outcome(stdout, 0, true).unwrap_err();
        assert!(matches!(err, RunnerError::MissingResult { .. }));
    }
}
