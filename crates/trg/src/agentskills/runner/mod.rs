pub mod availability;
pub mod claude_code;
pub mod codex;
pub mod cursor_agent;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::errors::SkillError;
use super::evals::{EvalCase, EvalError};
use super::outputs::ensure_outputs_dir;
use super::prompt::{build_eval_prompt, EvalPromptInput, SKILL_LINK_OLD, SKILL_LINK_WITH};
use super::redact::{redact_transcript_bytes, RedactedCommandLine};
use super::report::{ScenarioKind, SkillStaging};

#[derive(Debug, Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum Runner {
    #[value(name = "cursor-agent")]
    CursorAgent,
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
}

impl Runner {
    pub fn invoke(self, request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
        match self {
            Self::CursorAgent => cursor_agent::run(request),
            Self::ClaudeCode => claude_code::run(request),
            Self::Codex => codex::run(request),
        }
    }

    pub fn check_available(self) -> Result<(), EvalError> {
        availability::check_runner_available(self)
            .map(|_| ())
            .map_err(|unavailable| {
                let message = format!(
                    "Runner '{}' not found on PATH (looked for binary '{}'); {}",
                    unavailable.runner.display_name(),
                    unavailable.binary_name,
                    unavailable.install_hint()
                );
                EvalError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, message))
            })
    }

    pub fn program_name(self) -> &'static str {
        match self {
            Self::CursorAgent => "cursor-agent",
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }
}

pub struct EvalRunRequest<'a> {
    pub eval: &'a EvalCase,
    pub scenario: ScenarioKind,
    pub skill_md: &'a str,
    pub skill_path: &'a Path,
    pub old_skill_md: Option<&'a str>,
    pub old_skill_path: Option<&'a Path>,
    pub workspace_dir: &'a Path,
    pub transcript_path: &'a Path,
    pub stderr_path: &'a Path,
    pub runner_model: Option<&'a str>,
    pub timeout_secs: Option<u64>,
    pub skill_staging: SkillStaging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Failed,
    Timeout,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
        }
    }
}

pub const FAILURE_KIND_RUNNER: &str = "runner";

#[derive(Debug, Clone)]
pub struct EvalRunOutcome {
    pub status: RunStatus,
    pub failure_kind: Option<&'static str>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub final_text: String,
}

impl EvalRunOutcome {
    pub fn is_transient_failure(&self) -> bool {
        matches!(self.status, RunStatus::Timeout)
            || (matches!(self.status, RunStatus::Failed) && self.failure_kind == Some(FAILURE_KIND_RUNNER))
    }
}

#[derive(Debug)]
pub struct CapturedProcess {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
}

#[derive(Error, Debug)]
pub enum RunnerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("scenario '{0}' is not supported by this runner")]
    UnsupportedScenario(String),

    #[error("failed to spawn '{program}': {source}")]
    Spawn { program: String, source: std::io::Error },

    #[error("'{program}' produced invalid output: {detail}")]
    InvalidOutput { program: String, detail: String },
}

pub fn capture_subprocess(command: &mut Command, timeout: Option<Duration>) -> Result<CapturedProcess, RunnerError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = command.spawn().map_err(|source| RunnerError::Spawn {
        program: command.get_program().to_string_lossy().into_owned(),
        source,
    })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || read_pipe(stdout_pipe));
    let stderr_handle = thread::spawn(move || read_pipe_stderr(stderr_pipe));

    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = stdout_handle.join().unwrap_or_default();
                let stderr = stderr_handle.join().unwrap_or_default();
                return Ok(CapturedProcess {
                    stdout,
                    stderr,
                    exit_code: status.code(),
                    timed_out: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            None => {
                if let Some(limit) = timeout {
                    if start.elapsed() >= limit {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(CapturedProcess {
        stdout,
        stderr,
        exit_code: None,
        timed_out: true,
        duration_ms: timeout
            .map(|limit| limit.as_millis() as u64)
            .unwrap_or_else(|| start.elapsed().as_millis() as u64),
    })
}

fn read_pipe(pipe: Option<std::process::ChildStdout>) -> Vec<u8> {
    read_child_stream(pipe)
}

fn read_pipe_stderr(pipe: Option<std::process::ChildStderr>) -> Vec<u8> {
    read_child_stream(pipe)
}

fn read_child_stream<R: Read>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = pipe {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

pub fn timeout_duration(timeout_secs: Option<u64>) -> Option<Duration> {
    timeout_secs.map(Duration::from_secs)
}

pub fn runner_failure_outcome(duration_ms: u64, exit_code: Option<i32>, final_text: String) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Failed,
        failure_kind: Some(FAILURE_KIND_RUNNER),
        duration_ms,
        exit_code,
        total_tokens: None,
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
        final_text,
    }
}

pub fn timeout_outcome(timeout_ms: u64, exit_code: Option<i32>) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Timeout,
        failure_kind: Some(FAILURE_KIND_RUNNER),
        duration_ms: timeout_ms,
        exit_code,
        total_tokens: None,
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
        final_text: String::new(),
    }
}

pub fn completed_outcome(
    duration_ms: u64,
    exit_code: Option<i32>,
    total_tokens: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost_usd: Option<f64>,
    final_text: String,
) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Completed,
        failure_kind: None,
        duration_ms,
        exit_code,
        total_tokens,
        input_tokens,
        output_tokens,
        cost_usd,
        final_text,
    }
}

pub fn persist_runner_io(request: &EvalRunRequest, captured: &CapturedProcess) -> Result<(), RunnerError> {
    write_transcript(request.transcript_path, &captured.stdout)?;
    write_stderr(request.stderr_path, &captured.stderr)?;
    Ok(())
}

pub fn check_runner_version(program: &str, install_hint: &str) -> Result<(), EvalError> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(EvalError::from)?;
    if output.status.success() {
        return Ok(());
    }
    Err(runner_unavailable_error(program, install_hint, &output))
}

fn runner_unavailable_error(program: &str, install_hint: &str, output: &std::process::Output) -> EvalError {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    let message = if detail.is_empty() {
        format!("runner '{program}' is not available or failed its version check; {install_hint}")
    } else {
        format!("runner '{program}' is not available or failed its version check ({detail}); {install_hint}")
    };
    EvalError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, message))
}

#[derive(Debug)]
pub struct PreparedPrompt {
    pub prompt: String,
}

pub fn prepare_workspace(request: &EvalRunRequest) -> Result<PreparedPrompt, RunnerError> {
    std::fs::create_dir_all(request.workspace_dir)?;
    ensure_outputs_dir(request.workspace_dir)?;

    match request.scenario {
        ScenarioKind::WithSkill => {
            stage_skill_into_workspace(
                request.skill_path,
                request.workspace_dir,
                SKILL_LINK_WITH,
                request.skill_staging,
            )?;
            for relative in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
            }
        }
        ScenarioKind::WithoutSkill => {
            for relative in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
            }
        }
        ScenarioKind::OldSkill => {
            let old_skill_path = request.old_skill_path.ok_or_else(|| RunnerError::InvalidOutput {
                program: "trg".to_string(),
                detail: "old_skill scenario requires old_skill_path".to_string(),
            })?;
            request.old_skill_md.ok_or_else(|| RunnerError::InvalidOutput {
                program: "trg".to_string(),
                detail: "old_skill scenario requires old_skill_md".to_string(),
            })?;
            stage_skill_into_workspace(
                old_skill_path,
                request.workspace_dir,
                SKILL_LINK_OLD,
                request.skill_staging,
            )?;
            for relative in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
            }
        }
    }

    let skill_md_for_prompt = match request.scenario {
        ScenarioKind::WithSkill => Some(request.skill_md),
        ScenarioKind::WithoutSkill => None,
        ScenarioKind::OldSkill => request.old_skill_md,
    };

    let prompt = build_eval_prompt(EvalPromptInput {
        scenario: request.scenario,
        eval: request.eval,
        skill_md: skill_md_for_prompt,
    })
    .map_err(skill_error_to_runner)?;

    Ok(PreparedPrompt {
        prompt: prompt.into_string(),
    })
}

fn skill_error_to_runner(err: SkillError) -> RunnerError {
    RunnerError::InvalidOutput {
        program: "trg".to_string(),
        detail: err.to_string(),
    }
}

fn stage_skill_into_workspace(
    skill_path: &Path,
    workspace_dir: &Path,
    link_name: &str,
    staging: SkillStaging,
) -> std::io::Result<()> {
    let dest = workspace_dir.join(link_name.trim_end_matches('/'));
    if dest.exists() || dest.symlink_metadata().is_ok() {
        remove_staged_skill(&dest)?;
    }

    match staging {
        SkillStaging::Symlink => symlink_skill_into_workspace(skill_path, workspace_dir, link_name),
        SkillStaging::Copy => copy_skill_into_workspace(skill_path, &dest),
    }
}

fn remove_staged_skill(path: &Path) -> std::io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Dot-prefixed so the skill is hidden from default `ls`/glob and won't collide with
/// staged fixture paths or with a `skill/` directory the agent might create itself —
/// the workspace is the agent's task space; the skill is sidecar reference material.
#[cfg(unix)]
fn symlink_skill_into_workspace(skill_path: &Path, workspace_dir: &Path, link_name: &str) -> std::io::Result<()> {
    let link = workspace_dir.join(link_name.trim_end_matches('/'));
    let absolute = std::fs::canonicalize(skill_path)?;
    std::os::unix::fs::symlink(absolute, link)
}

#[cfg(not(unix))]
fn symlink_skill_into_workspace(skill_path: &Path, workspace_dir: &Path, link_name: &str) -> std::io::Result<()> {
    let dest = workspace_dir.join(link_name.trim_end_matches('/'));
    copy_skill_tree(skill_path, &dest)
}

fn copy_skill_into_workspace(skill_path: &Path, dest: &Path) -> std::io::Result<()> {
    copy_skill_tree(skill_path, dest)
}

/// Copy a skill directory, dereferencing symlinks so the destination is fully self-contained.
fn copy_skill_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    let metadata = std::fs::metadata(src)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_skill_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dest)?;
    }
    Ok(())
}

fn stage_eval_file(skill_path: &Path, workspace_dir: &Path, relative: &str) -> std::io::Result<()> {
    let source = skill_path.join(relative);
    if !source.exists() {
        return Ok(());
    }

    let dest = workspace_dir.join(relative);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if source.is_dir() {
        copy_dir_recursive(&source, &dest)?;
    } else {
        std::fs::copy(&source, &dest)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub fn write_transcript(transcript_path: &Path, raw_stdout: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = transcript_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let redacted = redact_transcript_bytes(raw_stdout);
    std::fs::write(transcript_path, redacted.into_inner())
}

pub fn write_stderr(stderr_path: &Path, raw_stderr: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = stderr_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let redacted = redact_transcript_bytes(raw_stderr);
    std::fs::write(stderr_path, redacted.into_inner())
}

pub fn write_runner_invocation_metadata(
    run_dir: &Path,
    command_line: RedactedCommandLine,
    env: BTreeMap<String, String>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(run_dir)?;
    std::fs::write(run_dir.join("cmd"), format!("{}\n", command_line.into_inner()))?;
    std::fs::write(run_dir.join("env.json"), serde_json::to_string_pretty(&env)?)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TimingFile {
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

pub fn write_timing_file(timing_path: &Path, outcome: &EvalRunOutcome) -> std::io::Result<()> {
    if let Some(parent) = timing_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = TimingFile {
        duration_ms: outcome.duration_ms,
        exit_code: outcome.exit_code,
        total_tokens: outcome.total_tokens,
        input_tokens: outcome.input_tokens,
        output_tokens: outcome.output_tokens,
        cost_usd: outcome.cost_usd,
    };
    std::fs::write(timing_path, serde_json::to_string_pretty(&body).unwrap())
}

pub type SkillDigest = BTreeMap<String, String>;

pub fn compute_skill_digest(skill_path: &Path) -> std::io::Result<SkillDigest> {
    let mut digest = BTreeMap::new();
    walk_and_hash(skill_path, skill_path, &mut digest)?;
    Ok(digest)
}

fn walk_and_hash(root: &Path, dir: &Path, digest: &mut SkillDigest) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_and_hash(root, &path, digest)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path)?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            digest.insert(relative, format!("sha256:{}", super::hex_encode(hasher.finalize())));
        }
    }
    Ok(())
}

pub fn detect_tampering(before: &SkillDigest, after: &SkillDigest) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (path, hash_before) in before {
        match after.get(path) {
            Some(hash_after) if hash_after == hash_before => {}
            Some(_) => changed.push(path.clone()),
            None => changed.push(path.clone()),
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    use crate::agentskills::evals::EvalCase;
    use crate::agentskills::redact::{is_secret_env_key, redact_command_args, redact_env};
    use tempfile::tempdir;

    fn make_case(files: Vec<String>) -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "files": files,
            "assertions": [],
        }))
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn test_request<'a>(
        case: &'a EvalCase,
        scenario: ScenarioKind,
        skill_md: &'a str,
        skill_path: &'a Path,
        workspace: &'a Path,
        transcript_path: &'a Path,
        stderr_path: &'a Path,
        old_skill_md: Option<&'a str>,
        old_skill_path: Option<&'a Path>,
    ) -> EvalRunRequest<'a> {
        EvalRunRequest {
            eval: case,
            scenario,
            skill_md,
            skill_path,
            old_skill_md,
            old_skill_path,
            workspace_dir: workspace,
            transcript_path,
            stderr_path,
            runner_model: None,
            timeout_secs: None,
            skill_staging: SkillStaging::Symlink,
        }
    }

    #[test]
    fn prepare_with_skill_symlinks_skill_and_prefixes_prompt() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/files/input.txt"), "hello").unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec!["evals/files/input.txt".to_string()]);
        let request = test_request(
            &case,
            ScenarioKind::WithSkill,
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );

        let prepared = prepare_workspace(&request).unwrap();
        assert!(prepared.prompt.contains("Skill available at: .skill/"));
        assert!(prepared.prompt.contains("name: test-skill"));
        assert!(!prepared.prompt.contains("# Skill"));
        assert!(prepared.prompt.contains("do the thing"));
        assert!(prepared.prompt.contains("outputs/"));
        assert!(workspace.join("outputs").is_dir());

        let link = workspace.join(".skill");
        #[cfg(unix)]
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(link.join("SKILL.md").is_file());
        assert!(workspace.join("evals/files/input.txt").is_file());
    }

    #[test]
    fn prepare_without_skill_returns_raw_prompt() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");

        let case = make_case(vec![]);
        let request = test_request(
            &case,
            ScenarioKind::WithoutSkill,
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );

        let prepared = prepare_workspace(&request).unwrap();
        assert!(prepared.prompt.starts_with("do the thing"));
        assert!(prepared.prompt.contains("outputs/"));
        assert!(!prepared.prompt.contains("Skill available at:"));
        assert!(!prepared.prompt.contains("Skill summary:"));
        assert!(!workspace.join(".skill").exists());
    }

    #[test]
    fn prepare_old_skill_symlinks_old_skill_not_current_and_uses_old_prompt() {
        let temp = tempdir().unwrap();
        let current_skill = temp.path().join("current-skill");
        let old_skill = temp.path().join("old-skill");
        std::fs::create_dir_all(current_skill.join("evals/files")).unwrap();
        std::fs::create_dir_all(&old_skill).unwrap();
        std::fs::write(
            current_skill.join("SKILL.md"),
            "---\nname: current-skill\ndescription: Current skill\n---\n# Current\n",
        )
        .unwrap();
        std::fs::write(
            old_skill.join("SKILL.md"),
            "---\nname: old-skill\ndescription: Old skill\n---\n# Old\n",
        )
        .unwrap();
        std::fs::write(current_skill.join("evals/files/input.txt"), "from-current").unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec!["evals/files/input.txt".to_string()]);
        let request = test_request(
            &case,
            ScenarioKind::OldSkill,
            "---\nname: current-skill\ndescription: Current skill\n---\n# Current\n",
            &current_skill,
            &workspace,
            &transcript,
            &stderr,
            Some("---\nname: old-skill\ndescription: Old skill\n---\n# Old\n"),
            Some(&old_skill),
        );

        let prepared = prepare_workspace(&request).unwrap();
        assert!(prepared.prompt.contains("Skill available at: .old-skill/"));
        assert!(prepared.prompt.contains("name: old-skill"));
        assert!(!prepared.prompt.contains("name: current-skill"));
        assert!(!prepared.prompt.contains("Skill available at: .skill/"));
        assert!(!prepared.prompt.contains("# Old\n"));
        assert!(!prepared.prompt.contains("# Current"));

        let link = workspace.join(".old-skill");
        #[cfg(unix)]
        {
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
            let target = std::fs::read_link(&link).unwrap();
            assert_eq!(target, std::fs::canonicalize(&old_skill).unwrap());
        }
        assert_eq!(
            std::fs::read_to_string(link.join("SKILL.md")).unwrap(),
            "---\nname: old-skill\ndescription: Old skill\n---\n# Old\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("evals/files/input.txt")).unwrap(),
            "from-current"
        );
    }

    #[test]
    fn old_skill_tampering_detection_is_scoped_to_old_skill_directory() {
        let temp = tempdir().unwrap();
        let current_skill = temp.path().join("current-skill");
        let old_skill = temp.path().join("old-skill");
        std::fs::create_dir_all(&current_skill).unwrap();
        std::fs::create_dir_all(&old_skill).unwrap();
        std::fs::write(current_skill.join("SKILL.md"), "current-stable").unwrap();
        std::fs::write(old_skill.join("SKILL.md"), "old-stable").unwrap();

        let old_before = compute_skill_digest(&old_skill).unwrap();
        std::fs::write(current_skill.join("SKILL.md"), "current-tampered").unwrap();
        let old_after_current_tampered = compute_skill_digest(&old_skill).unwrap();
        assert!(detect_tampering(&old_before, &old_after_current_tampered).is_empty());

        std::fs::write(old_skill.join("SKILL.md"), "old-tampered").unwrap();
        let old_after_old_tampered = compute_skill_digest(&old_skill).unwrap();
        assert_eq!(detect_tampering(&old_before, &old_after_old_tampered), vec!["SKILL.md"]);
    }

    #[test]
    fn prepare_old_skill_requires_old_skill_fields() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");

        let case = make_case(vec![]);
        let request = test_request(
            &case,
            ScenarioKind::OldSkill,
            "",
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );

        let err = prepare_workspace(&request).unwrap_err();
        assert!(matches!(err, RunnerError::InvalidOutput { .. }));
    }

    #[test]
    fn detect_tampering_flags_changes_additions_and_removals() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "original").unwrap();
        std::fs::write(skill.join("notes.md"), "keep").unwrap();

        let before = compute_skill_digest(&skill).unwrap();
        assert_eq!(before.len(), 2);

        std::fs::write(skill.join("SKILL.md"), "tampered").unwrap();
        std::fs::write(skill.join("added.md"), "new").unwrap();
        std::fs::remove_file(skill.join("notes.md")).unwrap();

        let after = compute_skill_digest(&skill).unwrap();
        let changed = detect_tampering(&before, &after);
        assert_eq!(changed, vec!["SKILL.md", "added.md", "notes.md"]);
    }

    #[test]
    fn detect_tampering_returns_empty_when_unchanged() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "stable").unwrap();
        let before = compute_skill_digest(&skill).unwrap();
        let after = compute_skill_digest(&skill).unwrap();
        assert!(detect_tampering(&before, &after).is_empty());
    }

    #[test]
    fn write_transcript_redacts_bearer_aws_github_and_jwt_tokens() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("transcript.jsonl");
        let github = "ghp_123456789012345678901234567890123456";
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let raw =
            format!("Authorization: Bearer abcdefghijklmnop\nkeys AKIAIOSFODNN7EXAMPLE and {github}\ntoken={jwt}\n");
        write_transcript(&path, raw.as_bytes()).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("Bearer abcdefghijklmnop"));
        assert!(!written.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!written.contains(github));
        assert!(!written.contains(jwt));
        assert!(written.contains("<redacted>"));
    }

    #[test]
    fn write_stderr_redacts_secrets() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("stderr.log");
        write_stderr(&path, b"stderr Bearer abcdefghijklmnop\n").unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("Bearer abcdefghijklmnop"));
        assert!(written.contains("<redacted>"));
    }

    #[test]
    fn write_runner_invocation_metadata_redacts_command_args_and_env() {
        let temp = tempdir().unwrap();
        let run_dir = temp.path().join("run-001");
        let github = "ghp_123456789012345678901234567890123456";
        let secret_key = "TRG_REDACT_TEST_OPENAI_API_KEY";
        std::env::set_var(secret_key, "sk-secret");
        std::env::set_var("TRG_REDACT_TEST_SAFE", "visible");

        write_runner_invocation_metadata(
            &run_dir,
            redact_command_args("codex", &["exec", "--api-key", github, "--model", "gpt-4"]),
            redact_env(),
        )
        .unwrap();

        let cmd = std::fs::read_to_string(run_dir.join("cmd")).unwrap();
        assert!(!cmd.contains(github));
        assert!(cmd.contains("--api-key"));
        assert!(cmd.contains("<redacted>"));

        let env: BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join("env.json")).unwrap()).unwrap();
        assert!(env.contains_key("TRG_REDACT_TEST_SAFE"));
        assert!(!env.contains_key(secret_key));
        assert!(!env
            .keys()
            .any(|key| is_secret_env_key(key) && key.starts_with("TRG_REDACT_TEST_")));

        std::env::remove_var(secret_key);
        std::env::remove_var("TRG_REDACT_TEST_SAFE");
    }
}
