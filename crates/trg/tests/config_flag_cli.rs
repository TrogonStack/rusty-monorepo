//! `--config` and `TRG_CONFIG` through the binary: which file is read, which
//! one wins, and that a path that is not there is never papered over by
//! reading the default file instead.

#![expect(
    clippy::unwrap_used,
    reason = "an integration test is test code end to end, so a failed setup step should fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// A config file declaring one keychain backend under `name`, so which file was
/// read shows in the list of declared backends an unknown one is refused with.
fn config_declaring(dir: &Path, file: &str, name: &str) -> PathBuf {
    let path = dir.join(file);
    fs::write(&path, format!("[secrets.backends.{name}]\nkind = \"keychain\"\n")).unwrap();
    path
}

struct Run {
    home: tempfile::TempDir,
    env: Option<PathBuf>,
    flag: Option<PathBuf>,
}

impl Run {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("trg")).unwrap();
        config_declaring(&home.path().join("trg"), "config.toml", "from-default");
        Self {
            home,
            env: None,
            flag: None,
        }
    }

    /// What the run printed on stderr, for a `get` against a backend no config
    /// declares, which refuses by naming the ones the chosen file declares.
    fn stderr(&self) -> String {
        let mut cmd = Command::cargo_bin("trg").unwrap();
        cmd.env("XDG_CONFIG_HOME", self.home.path());
        cmd.env_remove("TRG_CONFIG");
        if let Some(env) = &self.env {
            cmd.env("TRG_CONFIG", env);
        }
        if let Some(flag) = &self.flag {
            cmd.arg("--config").arg(flag);
        }
        cmd.args(["secret", "get", "--backend", "nope", "--path", "a", "--key", "k"]);
        let out = cmd.output().unwrap();
        assert!(!out.status.success(), "the lookup of an undeclared backend must fail");
        String::from_utf8(out.stderr).unwrap()
    }
}

#[test]
fn without_either_the_default_file_is_read() {
    let run = Run::new();
    assert!(run.stderr().contains("declared: from-default"), "{}", run.stderr());
}

#[test]
fn the_environment_variable_beats_the_default() {
    let mut run = Run::new();
    run.env = Some(config_declaring(run.home.path(), "env.toml", "from-env"));
    let stderr = run.stderr();
    assert!(stderr.contains("declared: from-env"), "{stderr}");
}

#[test]
fn the_flag_beats_the_environment_variable() {
    let mut run = Run::new();
    run.env = Some(config_declaring(run.home.path(), "env.toml", "from-env"));
    run.flag = Some(config_declaring(run.home.path(), "flag.toml", "from-flag"));
    let stderr = run.stderr();
    assert!(stderr.contains("declared: from-flag"), "{stderr}");
}

#[test]
fn the_flag_may_follow_the_subcommand() {
    let run = Run::new();
    let file = config_declaring(run.home.path(), "flag.toml", "from-flag");
    let out = Command::cargo_bin("trg")
        .unwrap()
        .env("XDG_CONFIG_HOME", run.home.path())
        .env_remove("TRG_CONFIG")
        .args([
            "secret",
            "get",
            "--backend",
            "nope",
            "--path",
            "a",
            "--key",
            "k",
            "--config",
        ])
        .arg(file)
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("declared: from-flag"), "{stderr}");
}

#[test]
fn a_missing_flag_path_is_an_error_and_not_a_reason_to_read_the_default() {
    let mut run = Run::new();
    let missing = run.home.path().join("absent.toml");
    run.flag = Some(missing.clone());
    let stderr = run.stderr();
    assert!(stderr.contains("absent.toml"), "{stderr}");
    assert!(!stderr.contains("from-default"), "{stderr}");
}

#[test]
fn a_missing_environment_path_is_an_error_and_not_a_reason_to_read_the_default() {
    let mut run = Run::new();
    run.env = Some(run.home.path().join("absent.toml"));
    let stderr = run.stderr();
    assert!(stderr.contains("absent.toml"), "{stderr}");
    assert!(!stderr.contains("from-default"), "{stderr}");
}

#[test]
fn an_empty_environment_variable_is_an_error_and_not_a_reason_to_read_the_default() {
    let mut run = Run::new();
    run.env = Some(PathBuf::new());
    let stderr = run.stderr();
    assert!(!stderr.contains("from-default"), "{stderr}");
}
