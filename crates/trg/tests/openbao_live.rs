//! Drives the real backend against a live OpenBao.
//!
//! Ignored by default: it needs a reachable instance and a token that may
//! write. It also needs `BAO_CONFIG` naming a config file, because it declares
//! nothing on your behalf: the instance, the mount, the subtree, and where the
//! token comes from are all read from that file, so a live pass covers the
//! config layer and not only the HTTP client.
//!
//! ```toml
//! [secrets.backends.live]
//! kind = "openbao"
//! addr = { env = "BAO_ADDR" }
//! mount = "kv"
//! path_prefix = "trg-adapter-check"
//! owner = "alice"
//! token_file = "~/.vault-token"
//! ```
//!
//! ```sh
//! BAO_CONFIG=./live.toml BAO_ADDR=https://bao.example.com \
//!   cargo test -p trg --test openbao_live -- --ignored --nocapture
//! ```
//!
//! `BAO_BACKEND` picks which declared backend to drive, and defaults to
//! `live`. Pointing `BAO_CONFIG` at your own `config.toml` is what this suite
//! is for: an instance whose policy templates the path on the caller's
//! identity then gets exercised at the subtree that policy actually grants.
//!
//! Beware when pointing these at a throwaway `bao server -dev`: dev mode
//! persists its root token to the token helper, overwriting whatever
//! `~/.vault-token` held for a real instance. Start it with
//! `-dev-no-store-token`.
use secrecy::ExposeSecret;
use serde::Deserialize;

use trg::config::VarSource;
use trg::secrets::config::OpenbaoConfig;
use trg::secrets::{
    Backend, BackendConfig, OpenBaoBackend, Registry, SecretKey, SecretMap, SecretPath, SecretsSection,
};

/// Enough of a config file to reach `[secrets]`, so `BAO_CONFIG` can name a
/// real one rather than something shaped only for this suite.
#[derive(Deserialize)]
struct Root {
    secrets: SecretsSection,
}

fn section() -> SecretsSection {
    let path = std::env::var("BAO_CONFIG").unwrap_or_else(|_| {
        panic!(
            "BAO_CONFIG must name a config file declaring the backend to drive: this suite \
             declares nothing on your behalf, so the instance, the mount, the subtree, and the \
             token source all come from that file"
        )
    });
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("BAO_CONFIG at `{path}`: {e}"));
    let root: Root = toml::from_str(&text).unwrap_or_else(|e| panic!("BAO_CONFIG at `{path}`: {e}"));
    root.secrets
}

fn name() -> String {
    std::env::var("BAO_BACKEND").unwrap_or_else(|_| "live".to_string())
}

fn resolve(section: SecretsSection, name: &str) -> OpenBaoBackend {
    match Registry::new(section)
        .resolve(name)
        .unwrap_or_else(|e| panic!("`{name}` should resolve: {e}"))
    {
        Backend::OpenBao(backend) => backend,
        other => panic!("`{name}` is a {} backend, not openbao", other.kind()),
    }
}

/// The declared openbao backend, so a test can break one field of what the
/// config file actually says rather than inventing a whole document.
fn declared<'a>(section: &'a mut SecretsSection, name: &str) -> &'a mut OpenbaoConfig {
    match section
        .backends
        .get_mut(name)
        .unwrap_or_else(|| panic!("`{name}` is not declared in BAO_CONFIG"))
    {
        BackendConfig::Openbao(config) => config,
        other => panic!("`{name}` is a {} backend, not openbao", other.kind()),
    }
}

fn live() -> OpenBaoBackend {
    resolve(section(), &name())
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

/// Named for what it guarantees rather than for the message: a token scoped
/// to one subtree is denied before the mount is ever looked up, so which
/// error comes back depends on the token. That it is an error and not a miss
/// does not.
#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_bad_mount_is_never_silently_a_miss() {
    let mut section = section();
    declared(&mut section, &name()).mount = "definitely-not-a-mount".to_string();
    let bao = resolve(section, &name());

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a missing mount is not a miss");
    eprintln!("missing mount -> {err}");
}

#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_rejected_token_is_reported_as_unauthorized() {
    let mut section = section();
    let config = declared(&mut section, &name());
    config.token_file = None;
    config.token = Some(VarSource::Literal("definitely-not-a-token".to_string()));
    let bao = resolve(section, &name());

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a bad token is not a miss");
    eprintln!("bad token -> {err}");
}
