use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::Runner;

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_PROBE_POLL: Duration = Duration::from_millis(50);

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
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(VERSION_PROBE_POLL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let output = child.wait_with_output().ok()?;
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
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
        let version = capture_version(Path::new("/bin/sh")).expect("sh should expose --version quickly");
        assert!(!version.is_empty());
    }
}
