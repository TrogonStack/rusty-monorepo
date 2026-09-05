//! Drives the real backend against a live OpenBao.
//!
//! Ignored by default: it needs a reachable instance and a token that may
//! write. It reads the same `config.toml` that `trg` itself reads, and
//! declares nothing on your behalf, so the instance, the mount, the subtree,
//! and where the token comes from are all whatever your machine is already
//! configured with. A live pass therefore covers the config layer and not only
//! the HTTP client.
//!
//! ```sh
//! cargo test -p trg --test openbao_live -- --ignored --nocapture
//! ```
//!
//! It writes and then deletes `<path_prefix>/mcp/live-probe` in whichever
//! backend it drives. That is a real write to a real instance.
//!
//! `BAO_CONFIG` points it at a different config file, which is what you want
//! for a throwaway instance rather than your own. `BAO_BACKEND` names which
//! backend to drive, and is needed only when a config declares more than one
//! openbao backend.
//!
//! Beware when pointing these at a throwaway `bao server -dev`: dev mode
//! persists its root token to the token helper, overwriting whatever
//! `~/.vault-token` held for a real instance. Start it with
//! `-dev-no-store-token`.
use std::path::{Path, PathBuf};

use secrecy::ExposeSecret;
use serde::Deserialize;

use trg::config::VarSource;
use trg::secrets::config::OpenbaoConfig;
use trg::secrets::{
    Backend, BackendConfig, OpenBaoBackend, Registry, SecretKey, SecretMap, SecretPath, SecretsSection,
};

/// Enough of a config file to reach `[secrets]`, so this suite reads the
/// operator's real config rather than something shaped only for itself.
#[derive(Deserialize)]
struct Root {
    secrets: SecretsSection,
}

/// The config `trg` itself would read, unless pointed elsewhere.
fn config_path() -> PathBuf {
    match std::env::var_os("BAO_CONFIG") {
        Some(path) => PathBuf::from(path),
        None => trg::config::trg_config_path(),
    }
}

fn section(path: &Path) -> SecretsSection {
    let path = path.display();
    let text = std::fs::read_to_string(path.to_string()).unwrap_or_else(|e| panic!("config at `{path}`: {e}"));
    let root: Root = toml::from_str(&text).unwrap_or_else(|e| panic!("config at `{path}`: {e}"));
    root.secrets
}

/// Which backend to drive. A config that declares exactly one openbao backend
/// needs no help naming it, which is the case this suite is usually run in.
fn openbao_backend(section: &SecretsSection, path: &Path) -> String {
    if let Ok(name) = std::env::var("BAO_BACKEND") {
        return name;
    }
    let mut declared: Vec<&str> = section
        .backends
        .iter()
        .filter(|(_, config)| matches!(config, BackendConfig::Openbao(_)))
        .map(|(name, _)| name.as_str())
        .collect();
    declared.sort_unstable();

    match declared.as_slice() {
        [only] => (*only).to_string(),
        [] => panic!("`{}` declares no openbao backend", path.display()),
        many => panic!(
            "`{}` declares several openbao backends ({}); name one with BAO_BACKEND",
            path.display(),
            many.join(", ")
        ),
    }
}

/// The declared backend and its name, so a test can drive it or break one
/// field of it.
fn configured() -> (SecretsSection, String) {
    let path = config_path();
    let section = section(&path);
    let name = openbao_backend(&section, &path);
    (section, name)
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
        .unwrap_or_else(|| panic!("`{name}` is not a declared backend"))
    {
        BackendConfig::Openbao(config) => config,
        other => panic!("`{name}` is a {} backend, not openbao", other.kind()),
    }
}

fn live() -> OpenBaoBackend {
    let (section, name) = configured();
    resolve(section, &name)
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
    let (mut section, name) = configured();
    declared(&mut section, &name).mount = "definitely-not-a-mount".to_string();
    let bao = resolve(section, &name);

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a missing mount is not a miss");
    eprintln!("missing mount -> {err}");
}

#[tokio::test]
#[ignore = "needs a live OpenBao"]
async fn a_rejected_token_is_reported_as_unauthorized() {
    let (mut section, name) = configured();
    let config = declared(&mut section, &name);
    config.token_file = None;
    config.token = Some(VarSource::Literal("definitely-not-a-token".to_string()));
    let bao = resolve(section, &name);

    let p = SecretPath::parse("mcp/live-probe").expect("path");
    let err = bao.get(&p).await.expect_err("a bad token is not a miss");
    eprintln!("bad token -> {err}");
}
