//! `trg doctor` through the binary, where the output contract actually lives.
//!
//! These drive a config with no `[secrets]` section, which is the shape every
//! macOS user had before backends existed and the one most likely to be run
//! against by someone who has not configured anything yet.

use std::fs;

use assert_cmd::Command;

fn doctor(config_home: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.arg("doctor");
    cmd.args(args);
    cmd.output().unwrap()
}

fn config_home_declaring_nothing() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("trg")).unwrap();
    fs::write(
        dir.path().join("trg/config.toml"),
        "[mcp.servers.example]\nurl = \"https://example.com/mcp\"\n",
    )
    .unwrap();
    dir
}

/// A format flag that only applies on some paths through a command is a format
/// flag a script cannot rely on.
#[test]
fn json_is_json_even_when_no_backend_was_declared() {
    let home = config_home_declaring_nothing();
    let out = doctor(home.path(), &["--format", "json"]);

    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout was not JSON: {e}\n{stdout}"));

    let backends = parsed["backends"].as_array().expect("a backends array");
    assert_eq!(backends.len(), 1, "{stdout}");
    assert_eq!(backends[0]["kind"], "keychain", "{stdout}");
}

/// Declaring nothing still means something is used, so the report has to name
/// it rather than report an empty run.
#[test]
fn the_backend_used_when_none_is_declared_is_still_reported_on() {
    let home = config_home_declaring_nothing();
    let out = doctor(home.path(), &[]);

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("(default)"), "{stdout}");
    assert!(stdout.contains("keychain"), "{stdout}");
    assert!(stdout.contains("platform"), "{stdout}");

    // The keychain backend only works on macOS, and the report says so either
    // way rather than passing everywhere.
    assert_eq!(
        out.status.code(),
        Some(i32::from(!cfg!(target_os = "macos"))),
        "{stdout}"
    );
}

#[test]
fn a_backend_that_was_never_declared_is_named_along_with_the_ones_that_were() {
    let home = config_home_declaring_nothing();
    let out = doctor(home.path(), &["--backend", "nosuch"]);

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("nosuch"), "{stderr}");
    assert_eq!(out.status.code(), Some(1));
}
