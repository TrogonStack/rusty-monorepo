//! `trg mcp proxy` and every other subcommand need stdout free for their own
//! protocol (JSON-RPC, JSON output, plain text); an OTLP exporter writing a
//! connection failure there would corrupt it. These drive the real binary
//! with an unreachable OTLP endpoint and check stdout is exactly what the
//! command would have produced with telemetry off.

use std::fs;

use assert_cmd::Command;

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

fn doctor_with_unreachable_otlp(config_home: &std::path::Path) -> std::process::Output {
    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", config_home);
    // Nothing listens on port 1, so the exporter fails fast with a connection
    // error rather than hanging on a timeout.
    cmd.env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1");
    cmd.arg("doctor");
    cmd.args(["--output-format", "json"]);
    cmd.output().unwrap()
}

#[test]
fn stdout_stays_clean_when_the_otlp_endpoint_is_unreachable() {
    let home = config_home_declaring_nothing();
    let out = doctor_with_unreachable_otlp(home.path());

    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout was not pure JSON: {e}\n{stdout}"));
    let backends = parsed["backends"].as_array().expect("a backends array");
    assert_eq!(backends.len(), 1, "{stdout}");

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
}
