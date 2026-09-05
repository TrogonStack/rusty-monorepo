//! Drives the real backend against a live OpenBao.
//!
//! Ignored by default: it needs a reachable instance and a token that may
//! write. Run it with `BAO_ADDR` and a `bao login` already done:
//!
//! ```sh
//! BAO_MOUNT=kv cargo test -p trg --test openbao_live -- --ignored --nocapture
//! ```
//!
//! The backend comes out of a config document through [`Registry`], the same
//! way a real run gets one, so a live pass covers the config layer and not
//! only the HTTP client. `BAO_MOUNT`, `BAO_PREFIX`, and `BAO_OWNER` fill in
//! the document, which lets an instance whose policy templates the path on the
//! caller's identity be exercised at the subtree that policy actually grants.
//! `BAO_CONFIG` replaces the document with a real config file, and
//! `BAO_BACKEND` names which of its backends to drive.
//!
//! Beware when pointing these at a throwaway `bao server -dev`: dev mode
//! persists its root token to the token helper, overwriting whatever
//! `~/.vault-token` held for a real instance. Start it with
//! `-dev-no-store-token`.
use secrecy::ExposeSecret;
use serde::Deserialize;

use trg::secrets::{Backend, OpenBaoBackend, Registry, SecretKey, SecretMap, SecretPath, SecretsSection};

/// Enough of a config file to reach `[secrets]`, so `BAO_CONFIG` can point at
/// a real one rather than at something shaped only for this suite.
#[derive(Deserialize)]
struct Root {
    secrets: SecretsSection,
}

const TOKEN_FILE: &str = r#"token_file = "~/.vault-token""#;

fn live() -> OpenBaoBackend {
    match std::env::var("BAO_CONFIG") {
        Ok(path) => {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("BAO_CONFIG at `{path}`: {e}"));
            resolve(
                &text,
                &std::env::var("BAO_BACKEND").unwrap_or_else(|_| "live".to_string()),
            )
        }
        Err(_) => resolve(&generated(&mount(), TOKEN_FILE), "live"),
    }
}

fn resolve(text: &str, name: &str) -> OpenBaoBackend {
    let root: Root = toml::from_str(text).expect("the live config should parse");
    match Registry::new(root.secrets)
        .resolve(name)
        .expect("the live backend should resolve")
    {
        Backend::OpenBao(backend) => backend,
        other => panic!("`{name}` is a {} backend, not openbao", other.kind()),
    }
}

fn generated(mount: &str, token: &str) -> String {
    let owner = match std::env::var("BAO_OWNER") {
        Ok(owner) => format!("owner = \"{owner}\"\n"),
        Err(_) => String::new(),
    };
    format!(
        "[secrets.backends.live]\n\
         kind = \"openbao\"\n\
         addr = {{ env = \"BAO_ADDR\" }}\n\
         mount = \"{mount}\"\n\
         path_prefix = \"{prefix}\"\n\
         {owner}\
         {token}\n\
         timeout_ms = 10000\n",
        prefix = prefix(),
    )
}

fn mount() -> String {
    std::env::var("BAO_MOUNT").unwrap_or_else(|_| "secret".to_string())
}

fn prefix() -> String {
    std::env::var("BAO_PREFIX").unwrap_or_else(|_| "trg-adapter-check".to_string())
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
    let bao = resolve(&generated("definitely-not-a-mount", TOKEN_FILE), "live");

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a missing mount is not a miss");
    eprintln!("missing mount -> {err}");
}

#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_rejected_token_is_reported_as_unauthorized() {
    let bao = resolve(&generated(&mount(), r#"token = "definitely-not-a-token""#), "live");

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a bad token is not a miss");
    eprintln!("bad token -> {err}");
}
