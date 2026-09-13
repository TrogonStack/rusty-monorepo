pub mod availability;
pub mod claude_code;
pub mod codex;
pub mod cursor_agent;
pub mod environment;
pub mod group;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::budget::HarnessPricing;
use super::errors::SkillError;
use super::evals::EVAL_SUITE_DIR_NAME;
use super::evals::{EvalCase, EvalError};
use super::outputs::ensure_outputs_dir;
use super::prompt::{build_eval_prompt, EvalPromptInput, SkillSummary, StagedSkillDir};
use super::redact::{redact_transcript_bytes, RedactedCommandLine, RedactedTranscript};
use super::report::{EnvironmentPolicy, ScenarioKind, SkillStaging};
use super::transcript::{write_normalized_transcript, StagedSkill, TranscriptFormat, WorkspaceBoundary};
use super::workspace_scaffold::{scaffold_workspace, ScaffoldFailure, ScaffoldPermission};
use environment::RunEnvironment;

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

    /// Whether this harness reports what a run cost.
    ///
    /// Only claude-code publishes a dollar figure alongside its result. The others report
    /// tokens and leave the price to whoever holds the rate card, so nothing downstream
    /// can total what a pass of theirs spent or hold it to a ceiling.
    pub fn pricing(self) -> HarnessPricing {
        match self {
            Self::ClaudeCode => HarnessPricing::Publishes,
            Self::CursorAgent | Self::Codex => HarnessPricing::Silent {
                harness: self.display_name().to_string(),
            },
        }
    }

    pub fn transcript_format(self) -> TranscriptFormat {
        match self {
            Self::CursorAgent => TranscriptFormat::CursorStreamJson,
            Self::ClaudeCode => TranscriptFormat::AnthropicStreamJson,
            Self::Codex => TranscriptFormat::CodexThreadJsonl,
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
    pub environment: EnvironmentPolicy,
    pub scaffold_permission: ScaffoldPermission,
}

impl EvalRunRequest<'_> {
    /// The run's own directory, which holds its transcript and, when isolated, its `HOME`.
    pub fn run_dir(&self) -> &Path {
        self.transcript_path.parent().unwrap_or(self.workspace_dir)
    }
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
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    group::lead_own_group(command);
    let start = Instant::now();
    let mut child = command.spawn().map_err(|source| RunnerError::Spawn {
        program: command.get_program().to_string_lossy().into_owned(),
        source,
    })?;
    let mut group = group::ProcessGroupGuard::led_by(&child);

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || read_pipe(stdout_pipe));
    let stderr_handle = thread::spawn(move || read_pipe_stderr(stderr_pipe));

    loop {
        match child.try_wait()? {
            Some(status) => {
                group.stop_leftovers();
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
                        group.terminate();
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

pub fn persist_runner_io(
    runner: Runner,
    request: &EvalRunRequest,
    captured: &CapturedProcess,
) -> Result<(), RunnerError> {
    let stdout = redact_transcript_bytes(&captured.stdout);
    write_transcript(request.transcript_path, &stdout)?;
    write_stderr(request.stderr_path, &captured.stderr)?;
    let normalized = runner
        .transcript_format()
        .normalize(
            runner.program_name(),
            &stdout,
            &WorkspaceBoundary::at(request.workspace_dir),
        )
        .staged_at(staged_skill(request)?);
    write_normalized_transcript(request.transcript_path, &normalized)?;
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
pub struct PreparedRun {
    pub prompt: String,
    pub environment: RunEnvironment,
}

pub fn prepare_workspace(request: &EvalRunRequest, runner: Runner) -> Result<PreparedRun, RunnerError> {
    reset_workspace(request.workspace_dir)?;
    ensure_outputs_dir(request.workspace_dir)?;

    // The scaffold goes first because it describes the directory the case is asking about,
    // and the skill and the case's fixtures are then staged into that directory. Running it
    // last would let it overwrite what the case declared, which is the opposite of what a
    // declaration is for.
    scaffold_workspace(
        request.eval.scaffold.as_ref(),
        request.scaffold_permission,
        request.skill_path,
        request.workspace_dir,
    )
    .map_err(scaffold_error_to_runner)?;

    if let Some((skill_path, staged_dir)) = skill_to_stage(request)? {
        stage_skill_into_workspace(
            skill_path,
            request.workspace_dir,
            staged_dir.as_str(),
            request.skill_staging,
        )?;
    }

    for relative in &request.eval.files {
        stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
    }

    let prompt = build_eval_prompt(EvalPromptInput {
        scenario: request.scenario,
        eval: request.eval,
        skill_md: skill_source(request)?.map(|(_, skill_md)| skill_md),
    })
    .map_err(skill_error_to_runner)?;

    let environment = RunEnvironment::prepare(runner, request.run_dir(), request.environment)?;

    Ok(PreparedRun {
        prompt: prompt.into_string(),
        environment,
    })
}

fn scaffold_error_to_runner(err: ScaffoldFailure) -> RunnerError {
    RunnerError::InvalidOutput {
        program: "trg".to_string(),
        detail: err.to_string(),
    }
}

/// Empty the workspace before anything is staged into it.
///
/// A retry is another run of the same case, and a case that declares state declares a
/// directory rather than a point to pile onto: replaying the scaffold over what the last
/// attempt left behind sets up a repository that is already half done, and scores a
/// directory nobody asked about. Removing the tree unlinks the staged skill's symlinks
/// instead of following them, so the clearing stops at the workspace.
fn reset_workspace(workspace_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(workspace_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::create_dir_all(workspace_dir)
}

fn skill_error_to_runner(err: SkillError) -> RunnerError {
    RunnerError::InvalidOutput {
        program: "trg".to_string(),
        detail: err.to_string(),
    }
}

/// The skill directory and its `SKILL.md` this scenario runs against, or `None`
/// for the arm that runs without a skill at all.
fn skill_source<'a>(request: &'a EvalRunRequest<'a>) -> Result<Option<(&'a Path, &'a str)>, RunnerError> {
    match request.scenario {
        ScenarioKind::WithoutSkill => Ok(None),
        ScenarioKind::WithSkill => Ok(Some((request.skill_path, request.skill_md))),
        ScenarioKind::OldSkill => {
            let path = request
                .old_skill_path
                .ok_or_else(|| missing_old_skill_input("old_skill_path"))?;
            let skill_md = request
                .old_skill_md
                .ok_or_else(|| missing_old_skill_input("old_skill_md"))?;
            Ok(Some((path, skill_md)))
        }
    }
}

/// The skill to stage and the workspace directory to stage it in, which the case
/// decides by saying whether its prompt announces the skill.
fn skill_to_stage<'a>(request: &'a EvalRunRequest<'a>) -> Result<Option<(&'a Path, StagedSkillDir)>, RunnerError> {
    let Some((skill_path, skill_md)) = skill_source(request)? else {
        return Ok(None);
    };
    let summary = SkillSummary::from_skill_md(skill_md).map_err(skill_error_to_runner)?;
    Ok(
        StagedSkillDir::for_run(request.scenario, request.eval.skill_disclosure, &summary.name)
            .map(|staged_dir| (skill_path, staged_dir)),
    )
}

/// What this run staged, for the transcript to carry to whoever grades it.
fn staged_skill(request: &EvalRunRequest) -> Result<StagedSkill, RunnerError> {
    Ok(match skill_to_stage(request)? {
        Some((_, directory)) => StagedSkill::At { directory },
        None => StagedSkill::Nothing,
    })
}

fn missing_old_skill_input(field: &str) -> RunnerError {
    RunnerError::InvalidOutput {
        program: "trg".to_string(),
        detail: format!("old_skill scenario requires {field}"),
    }
}

/// Is this top-level skill entry withheld from every run's workspace?
///
/// The eval suite is the answer key: it carries each case's `expected_output`, its
/// natural-language assertions, and its graders' literal `contains` text and `regex`
/// patterns. Staging it would let a run be scored on text it copied rather than work it
/// did, and the with-skill prompt points the agent straight at the staged directory, so
/// the suite is withheld in both staging modes.
///
/// Withholding costs a case nothing: the fixtures a case declares in `files` are staged
/// separately into the workspace root by `stage_eval_file`, which is the only part of the
/// suite directory a run is meant to see.
///
/// A version control directory is withheld for the same reason. When the skill is its own
/// checkout, its history still holds every revision of the suite, so a run that was handed
/// the working tree without `evals/` could ask git for the answer key instead. No eval run
/// needs a skill's history to do its work, so withholding it costs a case nothing.
///
/// Only the top level is filtered. A nested `evals/` deeper in the tree is the skill's own
/// content, not this suite, so it stages like anything else.
fn is_withheld_from_staging(entry_name: &std::ffi::OsStr) -> bool {
    WITHHELD_FROM_STAGING
        .iter()
        .any(|withheld| entry_name == std::ffi::OsStr::new(withheld))
}

const WITHHELD_FROM_STAGING: &[&str] = &[EVAL_SUITE_DIR_NAME, ".git", ".jj", ".hg", ".svn"];

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
/// staged fixture paths or with a `skill/` directory the agent might create itself.
/// The workspace is the agent's task space; the skill is sidecar reference material.
fn symlink_skill_into_workspace(skill_path: &Path, workspace_dir: &Path, link_name: &str) -> std::io::Result<()> {
    let dest = workspace_dir.join(link_name.trim_end_matches('/'));
    let absolute = std::fs::canonicalize(skill_path)?;
    std::fs::create_dir_all(&dest)?;
    for entry in std::fs::read_dir(&absolute)? {
        let entry = entry?;
        if is_withheld_from_staging(&entry.file_name()) {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), dest.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_skill_into_workspace(skill_path: &Path, dest: &Path) -> std::io::Result<()> {
    let skill = CopyableSkill::rooted_at(skill_path)?;
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(skill_path)? {
        let entry = entry?;
        if is_withheld_from_staging(&entry.file_name()) {
            continue;
        }
        copy_skill_tree(&skill, &entry.path(), &dest.join(entry.file_name()))?;
    }
    Ok(())
}

/// What a copy of a skill is allowed to draw from.
///
/// Copying dereferences links, which is what makes the staged copy self-contained, so the
/// question of what a link may point at is the question of what ends up in the workspace.
/// A link inside a skill can name the withheld suite as easily as it can name a file on the
/// operator's machine, and either one would arrive in the run's own directory with no link
/// left to give it away.
struct CopyableSkill {
    root: PathBuf,
    withheld: Vec<PathBuf>,
}

impl CopyableSkill {
    fn rooted_at(skill_path: &Path) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(skill_path)?;
        let withheld = WITHHELD_FROM_STAGING
            .iter()
            .map(|name| root.join(name))
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect();
        Ok(Self { root, withheld })
    }

    /// A path that cannot be resolved is a broken link, which there is nothing to copy from.
    fn may_copy(&self, path: &Path) -> bool {
        let Ok(resolved) = std::fs::canonicalize(path) else {
            return false;
        };
        resolved.starts_with(&self.root) && !self.withheld.iter().any(|withheld| resolved.starts_with(withheld))
    }
}

/// Copy a skill directory, dereferencing symlinks so the destination is fully self-contained.
///
/// Anything that resolves outside what the copy may draw from is left out, the same way the
/// suite itself is.
fn copy_skill_tree(skill: &CopyableSkill, src: &Path, dest: &Path) -> std::io::Result<()> {
    if !skill.may_copy(src) {
        return Ok(());
    }
    let metadata = std::fs::metadata(src)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_skill_tree(skill, &entry.path(), &dest.join(entry.file_name()))?;
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

pub fn write_transcript(transcript_path: &Path, stdout: &RedactedTranscript) -> std::io::Result<()> {
    if let Some(parent) = transcript_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(transcript_path, stdout.as_str())
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
#[serde(deny_unknown_fields)]
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
            environment: EnvironmentPolicy::Scrubbed,
            scaffold_permission: ScaffoldPermission::Withheld,
        }
    }

    const SCAFFOLD_SKILL_MD: &str = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";

    fn make_scaffolded_case(script: &str) -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "assertions": [],
            "scaffold": script,
        }))
        .unwrap()
    }

    fn write_scaffold(skill_path: &Path, relative: &str, body: &str) {
        let script = skill_path.join(relative);
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn scaffold_fixture(body: &str) -> (tempfile::TempDir, PathBuf, EvalCase) {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        write_scaffold(&skill_path, "evals/scaffold.sh", body);
        (temp, skill_path, make_scaffolded_case("evals/scaffold.sh"))
    }

    fn prepare_scaffolded(
        temp: &tempfile::TempDir,
        skill_path: &Path,
        case: &EvalCase,
        permission: ScaffoldPermission,
    ) -> (PathBuf, Result<PreparedRun, RunnerError>) {
        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let request = EvalRunRequest {
            scaffold_permission: permission,
            ..test_request(
                case,
                ScenarioKind::WithSkill,
                "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
                skill_path,
                &workspace,
                &transcript,
                &stderr,
                None,
                None,
            )
        };
        let prepared = prepare_workspace(&request, Runner::ClaudeCode);
        (workspace, prepared)
    }

    /// A case that says what state it is asking about gets that state, and gets it from trg
    /// rather than from a harness, so the same case states the same thing to every one.
    #[test]
    fn a_case_that_declares_state_runs_in_that_state() {
        let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\necho seeded > repo/README.md\n");

        let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

        prepared.expect("a granted scaffold runs");
        assert_eq!(
            std::fs::read_to_string(workspace.join("repo/README.md")).unwrap(),
            "seeded\n"
        );
    }

    /// A case scored against a workspace it never asked for reads as an answer about the
    /// skill when it is an answer about the wrong directory, so the run fails instead.
    #[test]
    fn a_declared_scaffold_nobody_allowed_fails_the_run_rather_than_running_without_it() {
        let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\n");

        let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Withheld);

        let error = prepared.expect_err("a withheld scaffold refuses the run");
        let message = error.to_string();
        assert!(
            message.contains("--allow-scaffold"),
            "expected the flag to be named: {message}"
        );
        assert!(
            message.contains("evals/scaffold.sh"),
            "expected the script to be named: {message}"
        );
        assert!(
            !workspace.join("repo").exists(),
            "nothing the script asked for was created"
        );
    }

    /// A scaffold that did not finish leaves a directory nobody described, so the run it was
    /// setting up cannot be reported as an answer about anything.
    #[test]
    fn a_scaffold_that_failed_fails_the_run_it_was_setting_up() {
        let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\necho 'no disk' >&2\nexit 3\n");

        let (_workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

        let message = prepared.expect_err("a failing scaffold refuses the run").to_string();
        assert!(message.contains("exit code 3"), "expected the exit code: {message}");
        assert!(
            message.contains("no disk"),
            "expected the script's own words: {message}"
        );
    }

    /// The case's own fixtures are staged after the scaffold, so a file the case declared is
    /// the file the run gets even when the script wrote to the same path.
    #[test]
    fn what_a_case_declares_outlives_what_its_scaffold_wrote() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/files/input.txt"), "declared").unwrap();
        write_scaffold(
            &skill_path,
            "evals/scaffold.sh",
            "#!/bin/sh\nmkdir -p evals/files\necho scaffolded > evals/files/input.txt\n",
        );
        let case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "files": ["evals/files/input.txt"],
            "assertions": [],
            "scaffold": "evals/scaffold.sh",
        }))
        .unwrap();

        let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

        prepared.expect("a granted scaffold runs");
        assert_eq!(
            std::fs::read_to_string(workspace.join("evals/files/input.txt")).unwrap(),
            "declared"
        );
    }

    /// The script is spawned as a path under the skill directory while the child changes
    /// into the workspace first, so a skill directory given relative to where trg was run
    /// has to be resolved before that change of directory rather than after it.
    #[test]
    fn a_scaffold_runs_when_the_skill_directory_is_a_relative_path() {
        let nearby = tempfile::TempDir::new_in(".").unwrap();
        let skill_path =
            PathBuf::from(nearby.path().file_name().expect("a name under the current directory")).join("skill");
        assert!(skill_path.is_relative(), "the point of the test is the relative path");
        std::fs::create_dir_all(&skill_path).unwrap();
        std::fs::write(skill_path.join("SKILL.md"), SCAFFOLD_SKILL_MD).unwrap();
        write_scaffold(
            &skill_path,
            "evals/scaffold.sh",
            "#!/bin/sh\necho seeded > seeded.txt\n",
        );
        let case = make_scaffolded_case("evals/scaffold.sh");

        let elsewhere = tempdir().unwrap();
        let workspace = elsewhere.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let request = EvalRunRequest {
            scaffold_permission: ScaffoldPermission::Granted,
            ..test_request(
                &case,
                ScenarioKind::WithSkill,
                SCAFFOLD_SKILL_MD,
                &skill_path,
                &workspace,
                &transcript,
                &stderr,
                None,
                None,
            )
        };

        prepare_workspace(&request, Runner::ClaudeCode).expect("a relative skill directory still finds its scaffold");

        assert_eq!(
            std::fs::read_to_string(workspace.join("seeded.txt")).unwrap(),
            "seeded\n"
        );
    }

    /// A retry is another run of the same case, so it is handed the state the case declared
    /// and not whatever the attempt before it left in the directory.
    #[test]
    fn a_workspace_prepared_again_does_not_keep_what_the_last_attempt_left() {
        let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\necho seeded > repo/README.md\n");

        let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);
        prepared.expect("a granted scaffold runs");
        std::fs::write(workspace.join("repo/README.md"), "rewritten by the agent").unwrap();
        std::fs::write(workspace.join("half-done.txt"), "from the attempt that failed").unwrap();

        let (_, again) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

        again.expect("the retry prepares the same workspace");
        assert!(
            !workspace.join("half-done.txt").exists(),
            "the retry asks the same question, so it starts from the same directory"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("repo/README.md")).unwrap(),
            "seeded\n"
        );
    }

    /// Clearing the workspace has to stop at the workspace. The staged skill is a tree of
    /// symlinks into the skill directory, and following them would take the skill down with
    /// the attempt that failed.
    #[test]
    fn clearing_a_workspace_does_not_reach_the_skill_it_staged() {
        let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\n:\n");

        let (_, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);
        prepared.expect("a granted scaffold runs");
        let (_, again) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

        again.expect("the retry prepares the same workspace");
        assert!(
            skill_path.join("SKILL.md").exists(),
            "the skill the next attempt reads is still there"
        );
        assert!(skill_path.join("evals/scaffold.sh").exists());
    }

    /// A case that declares nothing is untouched by any of this, and the permission it was
    /// never asked for does not decide whether it runs.
    #[test]
    fn a_case_that_declares_no_state_is_unaffected_by_the_permission() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        let case = make_case(vec![]);

        let (_workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Withheld);

        prepared.expect("a case with no scaffold needs no permission");
    }

    /// A scaffold outside the skill directory is refused when the manifest is read, not when
    /// a run reaches for it.
    #[test]
    fn a_scaffold_that_leaves_the_skill_directory_is_refused_at_parse_time() {
        let error = serde_json::from_value::<EvalCase>(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "assertions": [],
            "scaffold": "../../etc/setup.sh",
        }))
        .expect_err("a path that escapes is refused");
        assert!(
            error.to_string().contains("must stay inside the skill directory"),
            "got: {error}"
        );
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

        let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
        assert!(prepared.prompt.contains("Skill available at: .skill/"));
        assert!(prepared.prompt.contains("name: test-skill"));
        assert!(!prepared.prompt.contains("# Skill"));
        assert!(prepared.prompt.contains("do the thing"));
        assert!(prepared.prompt.contains("outputs/"));
        assert!(workspace.join("outputs").is_dir());

        let link = workspace.join(".skill");
        assert!(link.is_dir());
        assert!(link.join("SKILL.md").is_file());
        assert!(
            link.join("SKILL.md")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "staged entries are symlinks so staging stays cheap"
        );
        assert!(workspace.join("evals/files/input.txt").is_file());
    }

    /// What a run records as staged is read against the paths the run named, so it
    /// has to be the directory the workspace was staged at rather than a second guess
    /// at where staging put it.
    #[test]
    fn a_run_records_the_directory_it_staged_the_skill_at() {
        for disclosure in ["announced", "unannounced"] {
            let temp = tempdir().unwrap();
            let skill_path = temp.path().join("skill");
            std::fs::create_dir_all(&skill_path).unwrap();
            let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
            std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();

            let workspace = temp.path().join("ws");
            let transcript = workspace.join("transcript.jsonl");
            let stderr = workspace.join("stderr.log");
            let case: EvalCase = serde_json::from_value(serde_json::json!({
                "id": "case-1",
                "prompt": "do the thing",
                "expected_output": "done",
                "skill_disclosure": disclosure,
            }))
            .unwrap();
            let request = test_request(
                &case,
                ScenarioKind::WithSkill,
                skill_md,
                &skill_path,
                &workspace,
                &transcript,
                &stderr,
                None,
                None,
            );

            prepare_workspace(&request, Runner::ClaudeCode).unwrap();

            let StagedSkill::At { directory } = staged_skill(&request).unwrap() else {
                panic!("a with-skill run staged a skill");
            };
            assert!(
                workspace.join(directory.as_str()).join("SKILL.md").is_file(),
                "{disclosure} recorded {directory:?}, which is not where the skill was staged"
            );
        }
    }

    /// A triggering case asks whether the run reaches for the skill on its own, so
    /// the skill has to sit where a listing of the workspace reports it and the
    /// prompt has to stay silent about it.
    #[test]
    fn an_unannounced_case_stages_the_skill_in_plain_sight_and_names_it_nowhere() {
        for staging in [SkillStaging::Symlink, SkillStaging::Copy] {
            let temp = tempdir().unwrap();
            let skill_path = temp.path().join("skill");
            std::fs::create_dir_all(&skill_path).unwrap();
            let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
            std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();

            let workspace = temp.path().join("ws");
            let transcript = workspace.join("transcript.jsonl");
            let stderr = workspace.join("stderr.log");
            let case: EvalCase = serde_json::from_value(serde_json::json!({
                "id": "triggers-the-skill",
                "prompt": "do the thing",
                "expected_output": "done",
                "skill_disclosure": "unannounced",
            }))
            .unwrap();
            let mut request = test_request(
                &case,
                ScenarioKind::WithSkill,
                skill_md,
                &skill_path,
                &workspace,
                &transcript,
                &stderr,
                None,
                None,
            );
            request.skill_staging = staging;

            let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
            assert!(
                !prepared.prompt.contains("Skill available at:"),
                "{staging:?}: an unannounced case must not be told where the skill is"
            );
            assert!(
                !prepared.prompt.contains("test-skill"),
                "{staging:?}: an unannounced case must not be told which skill to use"
            );
            assert!(
                !workspace.join(".skill").exists(),
                "{staging:?}: the hidden link is what the prompt points at, and nothing points here"
            );
            assert!(
                workspace.join("skills/test-skill/SKILL.md").is_file(),
                "{staging:?}: the skill must be discoverable from the workspace"
            );
        }
    }

    /// The suite carries every case's expected output and its graders' literal patterns.
    /// A run that can read it can be scored on text it copied, so neither staging mode may
    /// put it in the workspace.
    #[test]
    fn staging_withholds_the_eval_suite_from_the_workspace() {
        for staging in [SkillStaging::Symlink, SkillStaging::Copy] {
            let temp = tempdir().unwrap();
            let skill_path = temp.path().join("skill");
            std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
            std::fs::write(
                skill_path.join("SKILL.md"),
                "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            )
            .unwrap();
            std::fs::write(skill_path.join("evals/files/input.txt"), "hello").unwrap();
            std::fs::write(
                skill_path.join("evals/evals.json"),
                r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
            )
            .unwrap();
            std::fs::create_dir_all(skill_path.join("evals/a/fixtures")).unwrap();
            std::fs::write(skill_path.join("evals/a/fixtures/seed.txt"), "seed").unwrap();

            let workspace = temp.path().join("ws");
            let transcript = workspace.join("transcript.jsonl");
            let stderr = workspace.join("stderr.log");
            let case = make_case(vec!["evals/files/input.txt".to_string()]);
            let mut request = test_request(
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
            request.skill_staging = staging;

            prepare_workspace(&request, Runner::ClaudeCode).unwrap();

            let staged_suite_dir = workspace.join(".skill/evals");
            assert!(
                !staged_suite_dir.exists(),
                "{staging:?}: the eval suite directory must not be staged"
            );
            assert!(
                std::fs::read_to_string(workspace.join(".skill/evals/evals.json")).is_err(),
                "{staging:?}: the manifest must be unreadable through the staged skill"
            );

            // The skill itself, and the fixture the case declared, are still there.
            assert!(
                workspace.join(".skill/SKILL.md").is_file(),
                "{staging:?}: the skill under test must still be staged"
            );
            assert!(
                workspace.join("evals/files/input.txt").is_file(),
                "{staging:?}: a declared fixture is staged separately and must survive"
            );
        }
    }

    fn staged_symlinks(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.symlink_metadata().unwrap().file_type().is_symlink() {
                found.push(path);
            } else if path.is_dir() {
                found.extend(staged_symlinks(&path));
            }
        }
        found
    }

    /// Keeping the suite out of the staged directory keeps it out of a listing, not out
    /// of the run: a staged symlink names the live skill directory, and the suite sits
    /// next to it, so following one entry is enough to read every expected output. The
    /// default mode has to leave nothing to follow.
    #[test]
    fn the_default_staging_mode_leaves_no_path_out_of_the_workspace() {
        assert_eq!(
            SkillStaging::default(),
            SkillStaging::Copy,
            "the mode a run gets without asking must be the hermetic one"
        );

        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("reference")).unwrap();
        std::fs::create_dir_all(skill_path.join("evals")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("reference/guide.md"), "guide").unwrap();
        std::fs::write(
            skill_path.join("evals/evals.json"),
            r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
        )
        .unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec![]);
        let mut request = test_request(
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
        request.skill_staging = SkillStaging::default();

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        let staged = workspace.join(".skill");
        assert!(
            staged_symlinks(&staged).is_empty(),
            "the default mode must stage nothing that names a path outside the workspace"
        );
        assert!(
            !staged.canonicalize().unwrap().join("../evals/evals.json").exists(),
            "walking out of the staged directory must not reach the suite"
        );
    }

    /// Why the cheap mode is the one you have to ask for.
    #[test]
    fn symlink_staging_tells_the_run_where_the_suite_lives() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(
            skill_path.join("evals/evals.json"),
            r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
        )
        .unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec![]);
        let mut request = test_request(
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
        request.skill_staging = SkillStaging::Symlink;

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        let staged_skill_md = workspace.join(".skill/SKILL.md");
        let target = std::fs::read_link(&staged_skill_md).unwrap();
        assert!(
            target.parent().unwrap().join("evals/evals.json").exists(),
            "symlink staging is documented as disclosing the skill's real location; if that stopped being true, make it the default"
        );
    }

    /// A nested `evals/` belongs to the skill's own content, so only the top level is filtered.
    #[test]
    fn staging_withholds_only_the_top_level_eval_suite() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals")).unwrap();
        std::fs::create_dir_all(skill_path.join("reference/evals")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
        std::fs::write(skill_path.join("reference/evals/guide.md"), "keep me").unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec![]);
        let mut request = test_request(
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
        request.skill_staging = SkillStaging::Copy;

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        assert!(!workspace.join(".skill/evals").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join(".skill/reference/evals/guide.md")).unwrap(),
            "keep me"
        );
    }

    fn skill_with_a_link(link_name: &str, target: &Path) -> (tempfile::TempDir, PathBuf) {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/evals.json"), "expected_output").unwrap();
        std::os::unix::fs::symlink(target, skill_path.join(link_name)).unwrap();
        (temp, skill_path)
    }

    fn stage_by_copy(skill_path: &Path, workspace: &Path) {
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec![]);
        let mut request = test_request(
            &case,
            ScenarioKind::WithSkill,
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            skill_path,
            workspace,
            &transcript,
            &stderr,
            None,
            None,
        );
        request.skill_staging = SkillStaging::Copy;
        prepare_workspace(&request, Runner::ClaudeCode).unwrap();
    }

    #[test]
    fn copy_staging_does_not_follow_a_link_to_the_withheld_suite() {
        let (temp, skill_path) = skill_with_a_link("notes", Path::new("evals"));
        let workspace = temp.path().join("ws");

        stage_by_copy(&skill_path, &workspace);

        assert!(
            !workspace.join(".skill/notes").exists(),
            "copying dereferences links, so a link naming the suite would deliver the answer key"
        );
    }

    #[test]
    fn copy_staging_does_not_follow_a_link_out_of_the_skill() {
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("operator-secret"), "secret").unwrap();
        let (temp, skill_path) = skill_with_a_link("elsewhere", outside.path());
        let workspace = temp.path().join("ws");

        stage_by_copy(&skill_path, &workspace);

        assert!(
            !workspace.join(".skill/elsewhere").exists(),
            "every path a copied skill offers has to stay inside the workspace"
        );
    }

    #[test]
    fn staging_withholds_the_history_the_eval_suite_is_recorded_in() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals")).unwrap();
        std::fs::create_dir_all(skill_path.join(".git/objects")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
        std::fs::write(skill_path.join(".git/objects/answer-key"), "expected_output").unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec![]);
        let mut request = test_request(
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
        request.skill_staging = SkillStaging::Copy;

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        assert!(
            !workspace.join(".skill/.git").exists(),
            "a skill that is its own checkout keeps every revision of the withheld suite in its history"
        );
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

        let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
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

        let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
        assert!(prepared.prompt.contains("Skill available at: .old-skill/"));
        assert!(prepared.prompt.contains("name: old-skill"));
        assert!(!prepared.prompt.contains("name: current-skill"));
        assert!(!prepared.prompt.contains("Skill available at: .skill/"));
        assert!(!prepared.prompt.contains("# Old\n"));
        assert!(!prepared.prompt.contains("# Current"));

        let link = workspace.join(".old-skill");
        assert!(link.is_dir());
        let target = std::fs::read_link(link.join("SKILL.md")).unwrap();
        assert_eq!(target, std::fs::canonicalize(old_skill.join("SKILL.md")).unwrap());
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

        let err = prepare_workspace(&request, Runner::ClaudeCode).unwrap_err();
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
    fn a_persisted_transcript_holds_no_bearer_aws_github_or_jwt_token() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("transcript.jsonl");
        let github = "ghp_123456789012345678901234567890123456";
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let raw =
            format!("Authorization: Bearer abcdefghijklmnop\nkeys AKIAIOSFODNN7EXAMPLE and {github}\ntoken={jwt}\n");
        write_transcript(&path, &redact_transcript_bytes(raw.as_bytes())).unwrap();
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

    /// A run is handed its prompt on the command line, never from the operator's
    /// keyboard. It also leads a background process group, where reading the controlling
    /// terminal stops the reader rather than failing it, so a harness that tried would
    /// hang the run instead of ending it.
    #[test]
    fn a_run_cannot_read_the_terminal_that_started_it() {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("if [ -t 0 ]; then echo tty; else echo notty; fi; cat");

        let captured = capture_subprocess(&mut command, Some(Duration::from_secs(10))).unwrap();

        assert!(
            !captured.timed_out,
            "a read of the run's stdin has to end the read, not the run"
        );
        assert_eq!(String::from_utf8_lossy(&captured.stdout).trim(), "notty");
    }

    #[test]
    fn a_timeout_takes_down_the_tools_the_harness_spawned() {
        let temp = tempdir().unwrap();
        let pid_file = temp.path().join("grandchild.pid");

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(format!("sleep 600 & echo $! > {}; sleep 600", pid_file.display()));

        let captured = capture_subprocess(&mut command, Some(Duration::from_secs(1))).unwrap();
        assert!(
            captured.timed_out,
            "the run had to be cut short for this to mean anything"
        );

        let grandchild: i32 = std::fs::read_to_string(&pid_file)
            .expect("the harness recorded the tool it spawned")
            .trim()
            .parse()
            .unwrap();
        assert!(
            !group::process_is_alive(grandchild),
            "a tool the harness spawned outlived the run it belonged to"
        );
    }

    fn wait_until_gone(pid: i32) -> bool {
        for _ in 0..40 {
            if !group::process_is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// A harness can finish its turn and still leave a tool running. That tool writes
    /// into a workspace that is about to be scored, so the run is not over until it is.
    #[test]
    fn a_harness_that_exits_does_not_leave_its_tools_running() {
        let temp = tempdir().unwrap();
        let pid_file = temp.path().join("leftover.pid");

        let mut command = Command::new("bash");
        command.arg("-c").arg(format!(
            "sleep 600 >/dev/null 2>&1 & echo $! > {}; exit 0",
            pid_file.display()
        ));

        let captured = capture_subprocess(&mut command, Some(Duration::from_secs(30))).unwrap();
        assert!(
            !captured.timed_out,
            "the harness had to exit on its own for this to mean anything"
        );

        let leftover: i32 = std::fs::read_to_string(&pid_file)
            .expect("the harness recorded the tool it spawned")
            .trim()
            .parse()
            .unwrap();
        assert!(
            wait_until_gone(leftover),
            "a tool the harness spawned outlived the run it belonged to"
        );
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
