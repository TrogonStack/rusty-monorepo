#![cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        acyclic_modules,
        reason = "Availability diagnostics and reconstructed telemetry share runner capability and usage types; extracting their common model is tracked separately"
    )
)]
use std::os::unix::fs::PermissionsExt;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
mod constants;
use constants::VCS_DIRS_WITHHELD_FROM_STAGING;
pub use constants::{FAILURE_KIND_RUNNER, FAILURE_KIND_UNSUPPORTED, RUN_STATUS_BUDGET_EXHAUSTED};
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod availability;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod capabilities;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod claude_code;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod codex;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod cursor_agent;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod environment;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod forwarding;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod group;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
mod harness_spans;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod stream;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub(crate) mod telemetry;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod usage;

#[cfg(test)]
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
mod fake;
#[cfg(test)]
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::budget::{HarnessPricing, RunCap, RunCaps, RunCost};
use super::case_directories::EVAL_CASE_PROMPT_FILE_NAME;
use super::companion_skills::{resolve_companions, CompanionError, StagedCompanion};
use super::errors::SkillError;
use super::evals::{EvalCase, EvalDirName, EvalError, EVAL_SUITE_MANIFEST_NAME};
use super::mocks::MaterializedMcpConfig;
use super::outputs::ensure_outputs_dir;
use super::prompt::{build_eval_prompt, EvalPromptInput, SkillName, SkillSummary, StagedSkillDir};
use super::redact::{redact_transcript_bytes, RedactedCommandLine, RedactedTranscript};
use super::report::{CacheTokens, EnvironmentPolicy, PermissionGrant, ScenarioKind, SkillStaging};
use super::tool_grant::ToolGrant;
use super::transcript::{
    write_normalized_transcript, StagedSkill, StagedSkillName, TranscriptFormat, WorkspaceBoundary,
};
use super::transcript_metrics::{ToolCallCount, TranscriptMetrics, TurnCount};
use super::workspace_scaffold::{scaffold_workspace, ScaffoldError, ScaffoldPermission};
use capabilities::HarnessControl;
use environment::{RecordedEnvironment, RunEnvironment};
pub use forwarding::TelemetryForwarding;
use harness_spans::LiveReconstruction;
use stream::{read_stamped, read_stamped_watching, StdoutTimeline};
use telemetry::{in_step, CliProcessSpan, FailureType};
use usage::HarnessTokenUsage;

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
        let result = match self {
            Self::CursorAgent => cursor_agent::run(request),
            Self::ClaudeCode => claude_code::run(request),
            Self::Codex => codex::run(request),
        };
        // Run unconditionally, regardless of which backend ran or whether it succeeded,
        // failed, or timed out: the harness has exited either way, and the `mcp-launch`
        // copy it was handed is a run-directory file this crate controls, so this is the
        // one place that can scrub it before the result is handed back.
        let scrub_result = request.mcp_config.as_ref().map(|mcp_config| {
            telemetry::in_step("scrub mcp launch config", || {
                mcp_config.scrub().map_err(RunnerError::from)
            })
        });
        let mut outcome = result?;
        if let Some(scrub_result) = scrub_result {
            scrub_result?;
        }
        // Checked here, once, regardless of which backend ran: permissions are only a
        // courtesy (an agent can delete a read-only file and recreate it in its place),
        // so content is re-verified against the source after every invocation.
        outcome.read_only_fixture_violations =
            telemetry::step_span("verify read-only fixtures").in_scope(|| verify_read_only_fixtures(request));
        Ok(outcome)
    }

    /// The grant a run under this harness actually operates with, which is not always
    /// the one requested.
    ///
    /// cursor-agent has exactly one non-interactive mode, `--force`, so any run under it
    /// executes unrestricted no matter what an operator asked for; claude-code and codex
    /// both offer a flag per grant, so they enforce exactly what was requested. This is
    /// the one place that answers what a run was actually allowed to do, so a report can
    /// say so rather than repeating the request back as if it were the outcome.
    pub fn effective_permission_grant(self, requested: PermissionGrant) -> PermissionGrant {
        match self {
            Self::ClaudeCode | Self::Codex => requested,
            Self::CursorAgent => PermissionGrant::Unrestricted,
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
    pub eval_dir: EvalDirName,
    pub old_skill_md: Option<&'a str>,
    pub old_skill_path: Option<&'a Path>,
    pub workspace_dir: &'a Path,
    pub transcript_path: &'a Path,
    pub stderr_path: &'a Path,
    pub runner_model: Option<&'a str>,
    pub timeout_secs: Option<u64>,
    pub skill_staging: SkillStaging,
    pub environment: EnvironmentPolicy,
    pub permission: PermissionGrant,
    pub scaffold_permission: ScaffoldPermission,
    /// The tool grant this run was actually given, once the operator's ceiling and the
    /// case's own declaration are combined. `None` means unrestricted: nobody asked for
    /// a tool allowlist. A runner that does not drive `ToolAllowlist` is never handed a
    /// `Some` here; a case or operator that asks for one against such a runner is
    /// refused earlier, before a runner is ever invoked.
    pub tool_grant: Option<ToolGrant>,
    /// Where the generated mock server table lives, in each format a harness reads, when
    /// the case declares mocks and the runner drives `McpServers`. Runners that do not
    /// drive that control simply ignore it; a case that declares mocks against one of those
    /// runners is refused earlier, before a runner is ever invoked.
    pub mcp_config: Option<MaterializedMcpConfig>,
    /// Whether the harness is handed what it needs to export its own telemetry.
    pub telemetry_forwarding: TelemetryForwarding,
    /// How many tool calls and turns the run may take before it is stopped.
    pub caps: RunCaps,
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
    /// Stopped for going past one of the run's caps on tool calls or turns.
    BudgetExhausted,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::BudgetExhausted => RUN_STATUS_BUDGET_EXHAUSTED,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalRunOutcome {
    pub status: RunStatus,
    pub failure_kind: Option<&'static str>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    /// What the harness said about this run's tokens, including any field it wrote in a
    /// shape that is not a count. The two travel together so a caller cannot publish the
    /// counts and leave the contradiction behind.
    pub tokens: HarnessTokenUsage,
    pub cost: Option<RunCost>,
    pub final_text: String,
    /// Read-only fixture paths whose staged copy no longer matches its source.
    pub read_only_fixture_violations: Vec<String>,
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
    /// When each line of `stdout` arrived.
    pub stdout_timeline: StdoutTimeline,
    pub started_at: SystemTime,
    pub pid: u32,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The cap the harness went past, when one of the run's caps is what stopped it.
    pub cap_reached: Option<RunCap>,
    /// When the harness was seen exiting, or was killed at its time limit or a cap.
    pub ended_at: SystemTime,
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

    #[error("'{program}' produced invalid output: {detail}")]
    InvalidOutputSource {
        program: String,
        detail: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

pub fn capture_subprocess(command: &mut Command, timeout: Option<Duration>) -> Result<CapturedProcess, RunnerError> {
    capture_capped(command, timeout, None)
}

/// The caps a harness is killed for going past, read off its stdout as it runs. A cap the
/// harness enforces itself is left to the harness, so the run ends the way it chose to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapWatch {
    runner: Runner,
    caps: RunCaps,
}

impl CapWatch {
    pub fn of(runner: Runner, caps: RunCaps) -> Option<Self> {
        Self::unassisted(
            runner,
            RunCaps {
                turns: caps
                    .turns
                    .filter(|_| runner.support(HarnessControl::TurnCap).flag().is_none()),
                ..caps
            },
        )
    }

    /// Watches every cap, for a process that speaks `runner`'s stream but was handed none
    /// of the flags `runner` would enforce a cap through.
    pub fn unassisted(runner: Runner, caps: RunCaps) -> Option<Self> {
        (!caps.is_unbounded()).then_some(Self { runner, caps })
    }
}

pub fn capture_capped(
    command: &mut Command,
    timeout: Option<Duration>,
    watch: Option<CapWatch>,
) -> Result<CapturedProcess, RunnerError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    group::lead_own_group(command);
    let start = Instant::now();
    let started_at = SystemTime::now();
    let mut child = command.spawn().map_err(|source| RunnerError::Spawn {
        program: command.get_program().to_string_lossy().into_owned(),
        source,
    })?;
    let pid = child.id();
    let mut group = group::ProcessGroupGuard::led_by(&child);

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let crossed: Arc<OnceLock<RunCap>> = Arc::default();
    let watching = Arc::clone(&crossed);
    let stdout_handle = thread::spawn(move || match watch {
        None => read_stamped(stdout_pipe),
        Some(watch) => {
            let mut live = LiveReconstruction::counting(watch.runner, started_at);
            read_stamped_watching(stdout_pipe, |line, at| {
                if watching.get().is_some() {
                    return;
                }
                live.observe_line(line, at);
                if let Some(cap) = watch.caps.crossed(
                    ToolCallCount::new(u64::try_from(live.tool_calls()).unwrap_or(u64::MAX)),
                    TurnCount::new(u64::try_from(live.model_calls()).unwrap_or(u64::MAX)),
                ) {
                    let _ = watching.set(cap);
                }
            })
        }
    });
    let stderr_handle = thread::spawn(move || read_child_stream(stderr_pipe));

    let ended_at = loop {
        match child.try_wait()? {
            Some(status) => {
                let ended_at = SystemTime::now();
                group.stop_leftovers();
                let (stdout, stdout_timeline) = stdout_handle.join().unwrap_or_default();
                let stdout_timeline = stdout_timeline.clamped_to(ended_at);
                let stderr = stderr_handle.join().unwrap_or_default();
                return Ok(CapturedProcess {
                    stdout,
                    stdout_timeline,
                    started_at,
                    pid,
                    stderr,
                    exit_code: status.code(),
                    timed_out: false,
                    cap_reached: crossed.get().copied(),
                    ended_at,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            None => {
                let stop = if let Some(cap) = crossed.get() {
                    tracing::info!(failure_kind = cap.failure_kind(), "SIGTERM");
                    Some(Stop::Cap(*cap))
                } else if let Some(limit) = timeout.filter(|limit| start.elapsed() >= *limit) {
                    tracing::info!(timeout_ms = limit.as_millis() as u64, "SIGTERM");
                    Some(Stop::Timeout(limit))
                } else {
                    None
                };
                if let Some(stop) = stop {
                    let terminating = Instant::now();
                    group.terminate();
                    tracing::info!(grace_ms = terminating.elapsed().as_millis() as u64, "SIGKILL");
                    let _ = child.kill();
                    let _ = child.wait();
                    break (SystemTime::now(), stop);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let (ended_at, stop) = ended_at;

    let (stdout, stdout_timeline) = stdout_handle.join().unwrap_or_default();
    let stdout_timeline = stdout_timeline.clamped_to(ended_at);
    let stderr = stderr_handle.join().unwrap_or_default();
    let (timed_out, cap_reached, duration_ms) = match stop {
        Stop::Timeout(limit) => (true, None, limit.as_millis() as u64),
        Stop::Cap(cap) => (false, Some(cap), start.elapsed().as_millis() as u64),
    };
    Ok(CapturedProcess {
        stdout,
        stdout_timeline,
        started_at,
        pid,
        stderr,
        exit_code: None,
        timed_out,
        cap_reached,
        ended_at,
        duration_ms,
    })
}

/// Why `trg` killed a harness that had not exited on its own.
#[derive(Debug, Clone, Copy)]
enum Stop {
    Timeout(Duration),
    Cap(RunCap),
}

fn read_child_stream<R: std::io::Read>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = pipe {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

pub fn timeout_duration(timeout_secs: Option<u64>) -> Option<Duration> {
    timeout_secs.map(Duration::from_secs)
}

/// A run nobody could finish still came from a harness with a pricing policy, and says the
/// same thing about cost a finished run of that harness would. A harness that prices
/// nothing has nothing to report for a failure either, so a reader totalling a mixed
/// history is not handed a failed run of it to add in as zero.
pub fn runner_failure_outcome(
    runner: Runner,
    duration_ms: u64,
    exit_code: Option<i32>,
    final_text: String,
) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Failed,
        failure_kind: Some(FAILURE_KIND_RUNNER),
        duration_ms,
        exit_code,
        tokens: HarnessTokenUsage::unreported(),
        cost: runner.pricing().price(None),
        final_text,
        read_only_fixture_violations: Vec::new(),
    }
}

/// A run stopped at one of its caps was still paid for up to that point, so it says what
/// its harness can say about cost, for the same reason a failed one does.
pub fn budget_exhausted_outcome(
    runner: Runner,
    cap: RunCap,
    duration_ms: u64,
    exit_code: Option<i32>,
) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::BudgetExhausted,
        failure_kind: Some(cap.failure_kind()),
        duration_ms,
        exit_code,
        tokens: HarnessTokenUsage::unreported(),
        cost: runner.pricing().price(None),
        final_text: String::new(),
        read_only_fixture_violations: Vec::new(),
    }
}

/// A run the clock ended says what its harness can say about cost, for the same reason a
/// failed one does.
pub fn timeout_outcome(runner: Runner, timeout_ms: u64, exit_code: Option<i32>) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Timeout,
        failure_kind: Some(FAILURE_KIND_RUNNER),
        duration_ms: timeout_ms,
        exit_code,
        tokens: HarnessTokenUsage::unreported(),
        cost: runner.pricing().price(None),
        final_text: String::new(),
        read_only_fixture_violations: Vec::new(),
    }
}

pub fn completed_outcome(
    duration_ms: u64,
    exit_code: Option<i32>,
    tokens: HarnessTokenUsage,
    cost: Option<RunCost>,
    final_text: String,
) -> EvalRunOutcome {
    EvalRunOutcome {
        status: RunStatus::Completed,
        failure_kind: None,
        duration_ms,
        exit_code,
        tokens,
        cost,
        final_text,
        read_only_fixture_violations: Vec::new(),
    }
}

pub fn persist_runner_io(
    runner: Runner,
    request: &EvalRunRequest,
    captured: &CapturedProcess,
) -> Result<(), RunnerError> {
    in_step("persist runner io", || {
        let stdout = telemetry::step_span("redact transcript").in_scope(|| redact_transcript_bytes(&captured.stdout));
        in_step("write transcript", || {
            Ok(write_transcript(request.transcript_path, &stdout)?)
        })?;
        in_step("write stderr", || {
            Ok(write_stderr(request.stderr_path, &captured.stderr)?)
        })?;
        in_step("normalize transcript", || {
            let normalized = runner
                .transcript_format()
                .normalize(
                    runner.program_name(),
                    &stdout,
                    &WorkspaceBoundary::at(request.workspace_dir),
                )
                .staged_at(staged_skill(request)?);
            Ok(write_normalized_transcript(request.transcript_path, &normalized)?)
        })
    })
}

pub fn check_runner_version(program: &str, install_hint: &str) -> Result<(), EvalError> {
    let probe = CliProcessSpan::start(program);
    let output = probe.span().in_scope(|| {
        let mut command = Command::new(program);
        command.arg("--version");
        crate::telemetry::propagation::inject_std_command(&mut command);
        command.output()
    });
    let output = output.map_err(|error| {
        probe.failed(FailureType::SpawnFailed);
        EvalError::from(error)
    })?;
    probe.exited(output.status.code());
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
    in_step("prepare workspace", || {
        in_step("reset workspace", || {
            reset_workspace(request.workspace_dir)?;
            ensure_outputs_dir(request.workspace_dir)?;
            Ok(())
        })?;

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

        in_step("stage skill", || {
            if let Some(plan) = skill_to_stage(request)? {
                stage_skill_into_workspace(
                    plan.source,
                    request.workspace_dir,
                    plan.directory.as_str(),
                    request.skill_staging,
                    &request.eval_dir,
                )?;
            }
            Ok(())
        })?;

        in_step("stage companions", || {
            for companion in companions_to_stage(request)? {
                stage_skill_into_workspace(
                    companion.source(),
                    request.workspace_dir,
                    companion.directory().as_str(),
                    request.skill_staging,
                    &request.eval_dir,
                )?;
            }
            Ok(())
        })?;

        in_step("stage fixtures", || {
            for fixture in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, fixture.as_str())?;
                if fixture.is_read_only() {
                    lock_fixture_permissions(&request.workspace_dir.join(fixture.as_path()))?;
                }
            }
            Ok(())
        })?;

        let prompt = in_step("build prompt", || {
            build_eval_prompt(EvalPromptInput {
                scenario: request.scenario,
                eval: request.eval,
                skill_md: skill_source(request)?.map(|(_, skill_md)| skill_md),
            })
            .map_err(skill_error_to_runner)
        })?;

        let environment = in_step("prepare environment", || {
            Ok(RunEnvironment::prepare(
                runner,
                request.run_dir(),
                request.environment,
                request.eval.env.as_ref(),
                request.telemetry_forwarding,
            )?)
        })?;

        Ok(PreparedRun {
            prompt: prompt.into_string(),
            environment,
        })
    })
}

fn scaffold_error_to_runner(err: ScaffoldError) -> RunnerError {
    RunnerError::InvalidOutputSource {
        program: "trg".to_string(),
        detail: err.to_string(),
        source: Box::new(err),
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
    RunnerError::InvalidOutputSource {
        program: "trg".to_string(),
        detail: err.to_string(),
        source: Box::new(err),
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

/// The skill a run stages, where it goes, and the name it answers to.
///
/// The name travels with the directory because a harness's own skill tool reports
/// only a name, and the announced directories do not carry one.
struct SkillStagingPlan<'a> {
    source: &'a Path,
    directory: StagedSkillDir,
    name: SkillName,
}

/// The skill to stage and the workspace directory to stage it in, which the case
/// decides by saying whether its prompt announces the skill.
fn skill_to_stage<'a>(request: &'a EvalRunRequest<'a>) -> Result<Option<SkillStagingPlan<'a>>, RunnerError> {
    let Some((skill_path, skill_md)) = skill_source(request)? else {
        return Ok(None);
    };
    let summary = SkillSummary::from_skill_md(skill_md).map_err(skill_error_to_runner)?;
    let name = SkillName::parse(&summary.name).map_err(skill_error_to_runner)?;
    Ok(
        StagedSkillDir::for_run(request.scenario, request.eval.skill_disclosure, &summary.name).map(|directory| {
            SkillStagingPlan {
                source: skill_path,
                directory,
                name,
            }
        }),
    )
}

/// The companions this run stages, in every arm including the one with no skill.
///
/// The without-skill arm gets them too, and that is what keeps the delta a delta. The arms
/// are supposed to differ in the skill under test and nothing else; staging distractors in
/// only one of them would make them differ in the skill under test plus however many other
/// skills the case declared, and the gap between the two would then be partly the gap
/// between a workspace with three skills and a workspace with none. With the companions in
/// both, what the control measures is a run that had everything the treatment had except
/// the skill being scored, which is the comparison the arm exists to make.
///
/// The old-skill arm follows for the same reason: it compares two versions of one skill,
/// and neither version is being asked to compete against a different set of distractors.
///
/// `skills/<name>/` is derived from the skill under test's own `SKILL.md` in every arm,
/// including the one that stages no skill, so a companion that would land on it is refused
/// on the strength of what the case declares rather than on which arm happened to run. The
/// arm's own staged directory is reserved alongside it, because the `old_skill` arm stages
/// that revision under the revision's own name and `--allow-skill-name-mismatch` lets that
/// name differ: without it, a companion wearing the older name would be staged over the
/// revision the arm exists to measure, and the arm would report on the companion.
fn companions_to_stage(request: &EvalRunRequest) -> Result<Vec<StagedCompanion>, RunnerError> {
    if request.eval.companion_skills.is_empty() {
        return Ok(Vec::new());
    }

    let summary = SkillSummary::from_skill_md(request.skill_md).map_err(skill_error_to_runner)?;
    let mut reserved = vec![StagedSkillDir::companion(&summary.name)];
    if let Some(plan) = skill_to_stage(request)? {
        if !reserved.iter().any(|taken| taken.as_str() == plan.directory.as_str()) {
            reserved.push(plan.directory);
        }
    }

    resolve_companions(
        &crate::fs::RealFS,
        request.skill_path,
        &request.eval.companion_skills,
        &reserved,
    )
    .map_err(companion_error_to_runner)
}

fn companion_error_to_runner(err: CompanionError) -> RunnerError {
    RunnerError::InvalidOutputSource {
        program: "trg".to_string(),
        detail: err.to_string(),
        source: Box::new(err),
    }
}

/// What this run staged, for the transcript to carry to whoever grades it.
fn staged_skill(request: &EvalRunRequest) -> Result<StagedSkill, RunnerError> {
    Ok(match skill_to_stage(request)? {
        Some(plan) => StagedSkill::At {
            directory: plan.directory,
            name: StagedSkillName::known(plan.name),
        },
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
///
/// Every sibling suite is withheld too, not only the one being run. Once a skill can hold
/// more than one suite, the directory this pass did not run is still an answer key, and its
/// cases routinely overlap the ones that did. Withholding only the resolved directory would
/// hand a run the very file it is being scored against under a different name.
fn is_withheld_from_staging(skill_path: &Path, entry_name: &std::ffi::OsStr, eval_dir: &EvalDirName) -> bool {
    if VCS_DIRS_WITHHELD_FROM_STAGING
        .iter()
        .any(|withheld| entry_name == std::ffi::OsStr::new(withheld))
    {
        return true;
    }
    if entry_name == std::ffi::OsStr::new(eval_dir.as_str()) {
        return true;
    }
    holds_an_eval_suite(&skill_path.join(entry_name))
}

/// Whether a directory is some suite, which is what makes it an answer key.
///
/// A manifest names it outright. The case-directory form has no manifest, so it is read as
/// a suite when its children are case directories carrying a prompt, which is the shape
/// `resolve_eval_suite` accepts.
fn holds_an_eval_suite(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    if path.join(EVAL_SUITE_MANIFEST_NAME).is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().is_dir() && entry.path().join(EVAL_CASE_PROMPT_FILE_NAME).is_file())
}

fn stage_skill_into_workspace(
    skill_path: &Path,
    workspace_dir: &Path,
    link_name: &str,
    staging: SkillStaging,
    eval_dir: &EvalDirName,
) -> std::io::Result<()> {
    let dest = workspace_dir.join(link_name.trim_end_matches('/'));
    if dest.exists() || dest.symlink_metadata().is_ok() {
        remove_staged_skill(&dest)?;
    }

    match staging {
        SkillStaging::Symlink => symlink_skill_into_workspace(skill_path, workspace_dir, link_name, eval_dir),
        SkillStaging::Copy => copy_skill_into_workspace(skill_path, &dest, eval_dir),
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
fn symlink_skill_into_workspace(
    skill_path: &Path,
    workspace_dir: &Path,
    link_name: &str,
    eval_dir: &EvalDirName,
) -> std::io::Result<()> {
    let dest = workspace_dir.join(link_name.trim_end_matches('/'));
    let absolute = std::fs::canonicalize(skill_path)?;
    std::fs::create_dir_all(&dest)?;
    for entry in std::fs::read_dir(&absolute)? {
        let entry = entry?;
        if is_withheld_from_staging(skill_path, &entry.file_name(), eval_dir) {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), dest.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_skill_into_workspace(skill_path: &Path, dest: &Path, eval_dir: &EvalDirName) -> std::io::Result<()> {
    let skill = CopyableSkill::rooted_at(skill_path, eval_dir)?;
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(skill_path)? {
        let entry = entry?;
        if is_withheld_from_staging(skill_path, &entry.file_name(), eval_dir) {
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
    fn rooted_at(skill_path: &Path, eval_dir: &EvalDirName) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(skill_path)?;
        let mut withheld: Vec<PathBuf> = VCS_DIRS_WITHHELD_FROM_STAGING
            .iter()
            .copied()
            .chain(std::iter::once(eval_dir.as_str()))
            .map(|name| root.join(name))
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if holds_an_eval_suite(&entry.path()) {
                    if let Ok(resolved) = std::fs::canonicalize(entry.path()) {
                        withheld.push(resolved);
                    }
                }
            }
        }
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

/// Clear write bits on a staged read-only fixture's file(s), leaving directories alone.
///
/// Directories are left writable on purpose: `reset_workspace` removes the whole workspace
/// between attempts by unlinking each entry, and unlinking an entry needs write permission
/// on the directory that holds it, not on the entry itself. Locking a fixture's own
/// directory down would make the next attempt's cleanup fail instead of protecting anything,
/// since a file's content is what this guards, not the tree that holds it.
fn lock_fixture_permissions(dest: &Path) -> std::io::Result<()> {
    lock_fixture_permissions_platform(dest)
}

#[cfg(unix)]
fn lock_fixture_permissions_platform(dest: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(dest)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(dest)? {
            let entry = entry?;
            lock_fixture_permissions_platform(&entry.path())?;
        }
    } else if metadata.is_file() {
        let mut perms = metadata.permissions();
        perms.set_mode(perms.mode() & !0o222);
        std::fs::set_permissions(dest, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn lock_fixture_permissions_platform(_dest: &Path) -> std::io::Result<()> {
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

/// Turn and tool counts read from a run's persisted transcript; `None` when the run
/// left no transcript to read.
pub fn measure_transcript(runner: Runner, transcript_path: &Path) -> Option<TranscriptMetrics> {
    let stdout = std::fs::read(transcript_path).ok()?;
    Some(harness_spans::measure(runner, &stdout))
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
    env: RecordedEnvironment,
) -> std::io::Result<()> {
    std::fs::create_dir_all(run_dir)?;
    std::fs::write(run_dir.join("cmd"), format!("{}\n", command_line.into_inner()))?;
    std::fs::write(run_dir.join("env.json"), serde_json::to_string_pretty(&env)?)
}

#[derive(Debug, Clone, Default)]
pub struct TimingFile {
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<CacheTokens>,
    /// What this run cost, or why nobody can say. Absent when the harness prices its runs
    /// and did not price this one, which is a different fact from a harness that prices
    /// none of them.
    pub cost: Option<RunCost>,
}

/// The shape a run's timing is written in, which is the shape every earlier release still
/// reads. `cost_usd` was that release's whole vocabulary for cost, so a run that carries a
/// price keeps publishing one; a run nobody priced has no number to put there, which is
/// what its absence has always meant.
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "TimingFile")]
struct SerializedTimingFile<'a> {
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_tokens: Option<CacheTokens>,
    /// What this run cost, or why nobody can say. Absent when the harness prices its runs
    /// and did not price this one, which is a different fact from a harness that prices
    /// none of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<&'a RunCost>,
    /// The price alone, for readers written before `cost` could say why a run carries
    /// none. Absent for exactly the runs that carry no price.
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

impl<'a> From<&'a TimingFile> for SerializedTimingFile<'a> {
    fn from(timing: &'a TimingFile) -> Self {
        Self {
            duration_ms: timing.duration_ms,
            exit_code: timing.exit_code,
            total_tokens: timing.total_tokens,
            input_tokens: timing.input_tokens,
            output_tokens: timing.output_tokens,
            cached_tokens: timing.cached_tokens,
            cost: timing.cost.as_ref(),
            cost_usd: timing.cost.as_ref().and_then(RunCost::usd),
        }
    }
}

impl Serialize for TimingFile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SerializedTimingFile::from(self).serialize(serializer)
    }
}

impl JsonSchema for TimingFile {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimingFile".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        SerializedTimingFile::json_schema(generator)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredTimingFile {
    duration_ms: u64,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cached_tokens: Option<CacheTokens>,
    #[serde(default)]
    cost: Option<RunCost>,
    /// Written by every release before a run's cost said which harness could not price
    /// it. Read as a priced run, because only a harness that prices its runs ever wrote
    /// a number here.
    #[serde(default)]
    cost_usd: Option<f64>,
}

impl<'de> Deserialize<'de> for TimingFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let declared = DeclaredTimingFile::deserialize(deserializer)?;
        Ok(Self {
            duration_ms: declared.duration_ms,
            exit_code: declared.exit_code,
            total_tokens: declared.total_tokens,
            input_tokens: declared.input_tokens,
            output_tokens: declared.output_tokens,
            cached_tokens: declared.cached_tokens,
            cost: declared
                .cost
                .or_else(|| declared.cost_usd.map(|usd| RunCost::Priced { usd })),
        })
    }
}

pub fn write_timing_file(timing_path: &Path, outcome: &EvalRunOutcome) -> std::io::Result<()> {
    if let Some(parent) = timing_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = TimingFile {
        duration_ms: outcome.duration_ms,
        exit_code: outcome.exit_code,
        total_tokens: outcome.tokens.total_tokens(),
        input_tokens: outcome.tokens.input_tokens(),
        output_tokens: outcome.tokens.output_tokens(),
        cached_tokens: outcome.tokens.cached_tokens(),
        cost: outcome.cost.clone(),
    };
    let json = serde_json::to_string_pretty(&body).map_err(std::io::Error::other)?;
    std::fs::write(timing_path, json)
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
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            digest.insert(relative, hash_file(&path)?);
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{}", super::hex_encode(hasher.finalize())))
}

/// Digest a fixture path, whether it is a single file or a directory, keyed the same way
/// `compute_skill_digest` keys a skill directory so the two sides can be compared with the
/// same `detect_tampering`.
fn hash_fixture(path: &Path) -> std::io::Result<SkillDigest> {
    let mut digest = BTreeMap::new();
    if path.is_dir() {
        walk_and_hash(path, path, &mut digest)?;
    } else {
        digest.insert(String::new(), hash_file(path)?);
    }
    Ok(digest)
}

/// Verify every read-only fixture's staged copy still matches its source after a run.
///
/// Permissions are only a courtesy: an agent that wants to change a read-only fixture can
/// delete it and write a fresh file in its place, which does not touch the mode of anything
/// that survives. Comparing content by hash is the guarantee that actually holds regardless
/// of how the mismatch came about, and reuses `detect_tampering`'s changed/missing/added
/// shape rather than inventing a second way to describe the same three outcomes.
fn verify_read_only_fixtures(request: &EvalRunRequest) -> Vec<String> {
    let mut violations = Vec::new();
    for fixture in &request.eval.files {
        if !fixture.is_read_only() {
            continue;
        }
        let source = request.skill_path.join(fixture.as_path());
        let staged = request.workspace_dir.join(fixture.as_path());
        let unchanged = match (hash_fixture(&source), hash_fixture(&staged)) {
            (Ok(before), Ok(after)) => detect_tampering(&before, &after).is_empty(),
            _ => false,
        };
        if !unchanged {
            violations.push(fixture.as_str().to_string());
        }
    }
    violations
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
