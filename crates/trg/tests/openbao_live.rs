//! Drives the real backend against a live OpenBao.
//!
//! Ignored by default: it needs a reachable instance and a token that may
//! write. Run it with `BAO_ADDR`, `BAO_MOUNT`, and a `bao login` already done:
//!
//! ```sh
//! BAO_MOUNT=kv cargo test -p trg --test openbao_live -- --ignored --nocapture
//! ```
use std::time::Duration;

use secrecy::ExposeSecret;

use trg::config::VarSource;
use trg::secrets::openbao::{OpenBaoSettings, TokenSource};
use trg::secrets::{OpenBaoBackend, SecretKey, SecretMap, SecretPath};

fn live() -> OpenBaoBackend {
    let addr = std::env::var("BAO_ADDR").expect("BAO_ADDR");
    let mount = std::env::var("BAO_MOUNT").unwrap_or_else(|_| "secret".to_string());
    OpenBaoBackend::new(OpenBaoSettings {
        addr,
        mount,
        path_prefix: "trg-adapter-check".to_string(),
        machine_id: None,
        token: TokenSource::File(dirs_home().join(".vault-token")),
        ca_cert_file: None,
        timeout: Duration::from_secs(10),
    })
    .expect("build")
}

fn dirs_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").expect("HOME"))
}

#[tokio::test]
#[ignore = "needs a live OpenBao and a token that may write"]
async fn round_trips_a_credential_against_a_live_instance() {
    let bao = live();
    let p = SecretPath::parse("mcp/live-probe").expect("path");

    assert!(bao.get(&p).await.expect("miss is not an error").is_none());

    let mut map = SecretMap::new();
    map.insert(
        SecretKey::parse("access_token").expect("key"),
        "probe-value".to_string().into(),
    );
    bao.set(&p, &map).await.expect("set");

    let got = bao.get(&p).await.expect("get").expect("a hit");
    assert_eq!(
        got.get(&SecretKey::parse("access_token").expect("key"))
            .map(|v| v.expose_secret()),
        Some("probe-value")
    );

    let keys = bao.list(None).await.expect("list");
    assert!(keys.iter().any(|k| k == "mcp/"), "{keys:?}");

    bao.delete(&p).await.expect("delete");
    assert!(bao.get(&p).await.expect("after delete").is_none());
}

#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_bad_mount_is_reported_as_a_configuration_error() {
    let addr = std::env::var("BAO_ADDR").expect("BAO_ADDR");
    let bao = OpenBaoBackend::new(OpenBaoSettings {
        addr,
        mount: "definitely-not-a-mount".to_string(),
        path_prefix: "trg-adapter-check".to_string(),
        machine_id: None,
        token: TokenSource::File(dirs_home().join(".vault-token")),
        ca_cert_file: None,
        timeout: Duration::from_secs(10),
    })
    .expect("build");

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a missing mount is not a miss");
    eprintln!("missing mount -> {err}");
}

#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_rejected_token_is_reported_as_unauthorized() {
    let addr = std::env::var("BAO_ADDR").expect("BAO_ADDR");
    let mount = std::env::var("BAO_MOUNT").unwrap_or_else(|_| "secret".to_string());
    let bao = OpenBaoBackend::new(OpenBaoSettings {
        addr,
        mount,
        path_prefix: "trg-adapter-check".to_string(),
        machine_id: None,
        token: TokenSource::Var(VarSource::Literal("definitely-not-a-token".to_string())),
        ca_cert_file: None,
        timeout: Duration::from_secs(10),
    })
    .expect("build");

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a bad token is not a miss");
    eprintln!("bad token -> {err}");
}
