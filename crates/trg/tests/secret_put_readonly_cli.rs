//! `trg secret put` against a read-only backend, through the binary.
//!
//! A read-only backend has to be rejected before anything else about the
//! write is attempted, stdin included: a caller piping a secret in expects
//! the pipe to matter only once `trg` has confirmed there is somewhere to
//! put it.

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use std::process::Command;

fn config_home_with_onepassword_backend() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("trg")).unwrap();
    std::fs::write(
        dir.path().join("trg/config.toml"),
        "[secrets.backends.op]\nkind = \"onepassword\"\naccount = \"my.1password.com\"\n",
    )
    .unwrap();
    dir
}

fn put_args() -> [&'static str; 8] {
    [
        "secret",
        "put",
        "--backend",
        "op",
        "--path",
        "Ops/deploy-keys",
        "--key",
        "TOKEN",
    ]
}

#[test]
fn a_read_only_backend_rejects_a_put() {
    let home = config_home_with_onepassword_backend();
    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", home.path());
    cmd.args(put_args());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();

    // Give it a value to write, in case the rejection did not come first.
    // If the backend is correctly rejected before stdin is read, this write
    // is simply never consumed.
    use std::io::Write;
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(b"a-value\n");
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("only reads from"), "{stderr}");
}

/// The regression this guards: `put` used to read stdin before checking
/// whether the backend was writable, so a read-only backend would hang
/// waiting on a pipe that was never going to supply anything relevant, or
/// silently consume a secret meant for somewhere else. Leaving the write end
/// of stdin open without writing or closing it reproduces exactly that hang
/// if the check and the read are ever reordered back.
#[test]
fn a_read_only_backend_rejects_a_put_without_reading_stdin() {
    let home = config_home_with_onepassword_backend();
    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", home.path());
    cmd.args(put_args());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();

    let _stdin = child.stdin.take().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "put did not exit while stdin was left open and unwritten; \
             it must have tried to read stdin before checking writability"
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    assert!(!status.success());
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert!(stderr.contains("only reads from"), "{stderr}");
}
