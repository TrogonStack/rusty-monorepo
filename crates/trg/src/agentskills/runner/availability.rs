use std::os::unix::fs::PermissionsExt;
mod constants;
use constants::{VERSION_PROBE_POLL, VERSION_PROBE_TIMEOUT};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::telemetry::{CliProcessSpan, FailureType};
use super::Runner;
use crate::telemetry::propagation::inject_std_command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerProbe {
    pub binary_path: PathBuf,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerUnavailable {
    pub runner: Runner,
    pub binary_name: String,
    pub searched_paths: Vec<String>,
}

impl RunnerUnavailable {
    pub fn install_hint(&self) -> &'static str {
        match self.runner {
            Runner::Codex => "install Codex CLI and ensure `codex` is on PATH (see docs/how-to/troubleshooting.md#runner-setup)",
            Runner::ClaudeCode => {
                "install Claude Code and ensure `claude` is on PATH (see docs/how-to/troubleshooting.md#runner-setup)"
            }
            Runner::CursorAgent => {
                "install Cursor Agent CLI and ensure `cursor-agent` is on PATH (see docs/how-to/troubleshooting.md#runner-setup)"
            }
        }
    }
}

impl Runner {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::CursorAgent => "cursor-agent",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

pub fn check_runner_available(runner: Runner) -> Result<RunnerProbe, RunnerUnavailable> {
    let path = std::env::var("PATH").unwrap_or_default();
    check_runner_available_on_path(runner, &path)
}

pub fn check_runner_available_on_path(runner: Runner, path: &str) -> Result<RunnerProbe, RunnerUnavailable> {
    let binary_name = runner.program_name().to_string();
    let path_dirs: Vec<PathBuf> = if path.is_empty() {
        Vec::new()
    } else {
        std::env::split_paths(path).collect()
    };
    let searched_paths: Vec<String> = path_dirs
        .iter()
        .map(|entry| entry.to_string_lossy().into_owned())
        .collect();

    let binary_path = path_dirs
        .into_iter()
        .find_map(|dir| {
            let candidate = dir.join(&binary_name);
            is_executable(&candidate).then_some(candidate)
        })
        .ok_or(RunnerUnavailable {
            runner,
            binary_name,
            searched_paths,
        })?;

    let version = capture_version(&binary_path);
    Ok(RunnerProbe { binary_path, version })
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub fn eprint_runner_unavailable(err: &RunnerUnavailable) {
    eprintln!("Runner '{}' not found on PATH.", err.runner.display_name());
    eprintln!("Looked for binary: {}", err.binary_name);
    let preview: Vec<_> = err.searched_paths.iter().take(5).cloned().collect();
    if preview.is_empty() {
        eprintln!("PATH directories searched: (empty)");
    } else {
        eprintln!("PATH directories searched: {}", preview.join(", "));
    }
    eprintln!("Install instructions: {}", err.install_hint());
}

fn capture_version(binary: &Path) -> Option<String> {
    let executable = binary
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let probe = CliProcessSpan::start(&executable);
    probe.span().in_scope(|| run_version_probe(binary, &probe))
}

fn run_version_probe(binary: &Path, probe: &CliProcessSpan) -> Option<String> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    inject_std_command(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            probe.failed(FailureType::SpawnFailed);
            return None;
        }
    };
    probe.spawned(child.id());

    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    probe.failed(FailureType::Timeout);
                    return None;
                }
                thread::sleep(VERSION_PROBE_POLL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                probe.failed(FailureType::Io);
                return None;
            }
        }
    }

    let output = child.wait_with_output().ok()?;
    probe.exited(output.status.code());
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let version = text.lines().next()?.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "availability_tests.rs"]
mod tests;
