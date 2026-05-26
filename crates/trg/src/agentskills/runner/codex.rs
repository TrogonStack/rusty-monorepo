use std::process::Command;
use std::time::Instant;

use super::{
    prepare_workspace, write_timing_file, write_transcript, EvalRunOutcome, EvalRunRequest, RunStatus, RunnerError,
};

const PROGRAM: &str = "codex";

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request)?;
    let final_text_path = request.workspace_dir.join(".trg-codex-final.txt");

    let mut command = Command::new(PROGRAM);
    command
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("-s")
        .arg("workspace-write")
        .arg("-C")
        .arg(request.workspace_dir)
        .arg("-o")
        .arg(&final_text_path);

    if let Some(model) = request.runner_model {
        command.arg("-m").arg(model);
    }

    command.arg(&prepared.prompt);

    let start = Instant::now();
    let output = command.output().map_err(|source| RunnerError::Spawn {
        program: PROGRAM.to_string(),
        source,
    })?;
    let wall_ms = start.elapsed().as_millis() as u64;

    write_transcript(request.transcript_path, &output.stdout)?;

    let final_text = std::fs::read_to_string(&final_text_path).unwrap_or_default();
    let outcome = parse_outcome(&output.stdout, wall_ms, output.status.success(), final_text)?;
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

fn parse_outcome(
    stdout: &[u8],
    wall_ms: u64,
    exit_ok: bool,
    final_text: String,
) -> Result<EvalRunOutcome, RunnerError> {
    let text = std::str::from_utf8(stdout).map_err(|_| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail: "stdout was not valid UTF-8".to_string(),
    })?;

    let mut terminal: Option<serde_json::Value> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("turn.completed") {
            terminal = Some(value);
        }
    }

    let terminal = terminal.ok_or_else(|| RunnerError::MissingResult {
        program: PROGRAM.to_string(),
    })?;

    let usage = terminal.get("usage");
    let input_tokens = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64());
    let cached_input = usage
        .and_then(|u| u.get("cached_input_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (None, None) => None,
        (i, o) => Some(i.unwrap_or(0) + o.unwrap_or(0) + cached_input.unwrap_or(0)),
    };

    Ok(EvalRunOutcome {
        status: if exit_ok {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        },
        duration_ms: wall_ms,
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
    fn parses_turn_completed_event() {
        let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40,"cached_input_tokens":10}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, "final".to_string()).unwrap();
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 5000);
        assert_eq!(outcome.input_tokens, Some(120));
        assert_eq!(outcome.output_tokens, Some(40));
        assert_eq!(outcome.total_tokens, Some(170));
        assert_eq!(outcome.final_text, "final");
    }

    #[test]
    fn missing_terminal_event_with_success_errors() {
        let stdout = br#"{"type":"thread.started"}
"#;
        let err = parse_outcome(stdout, 0, true, String::new()).unwrap_err();
        assert!(matches!(err, RunnerError::MissingResult { .. }));
    }
}
