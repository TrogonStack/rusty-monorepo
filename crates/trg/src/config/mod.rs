//! `~/.config/trg/config.toml` loader for `trg mcp *` (and future subcommands).
//!
//! Env-backed inputs are declared once in `[mcp.servers.<name>.vars]` as
//! `VarSource` entries (literal string, `{ env, default? }` table, or
//! `{ backend, path, key }` table naming a secrets backend). The
//! server's `url` and each header value (`VarTemplate`) accept three shapes:
//!   - a TOML string (literal),
//!   - a `{ var = "<name>" }` reference to a `vars` entry,
//!   - a TOML array mixing the above two, concatenated in order.
//!
//! Inline `{ env = "..." }` is rejected outside the `vars` table.

#[cfg(test)]
mod doc_examples;
mod var;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use http::HeaderName;
pub use secrecy::SecretString;
use serde::Deserialize;
pub use var::{FetchedSecrets, SecretVar, Segment, VarRef, VarResolveError, VarSource, VarTemplate};

use crate::secrets::SecretsSection;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Toml(#[from] toml::de::Error),

    #[error("config file not found at `{0}`")]
    NotFound(PathBuf),

    #[error("config file at `{path}` could not be read: {cause}")]
    Unreadable { path: PathBuf, cause: String },

    #[error("no `[mcp.servers]` section in config")]
    NoMcpServers,

    #[error("unknown MCP server `{name}` — known: {available}")]
    UnknownServer { name: String, available: String },

    #[error("could not decode header `{name}`: {cause}")]
    InvalidHeaderValue { name: String, cause: String },

    #[error("MCP server `url` must not be empty")]
    EmptyUrl,

    #[error("variable resolution failed: {0}")]
    VarResolve(#[from] VarResolveError),

    #[error("invalid header name `{0}`: {1}")]
    InvalidHeaderName(String, String),

    #[error("duplicate header `{name}` collides with `{existing}` after canonicalization")]
    DuplicateHeader { name: String, existing: String },

    #[error("no `[exec]` entries in config")]
    NoExecEntries,

    #[error("unknown exec entry `{name}` — known: {available}")]
    UnknownExecEntry { name: String, available: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRoot {
    #[serde(default)]
    mcp: Option<McpSection>,
    #[serde(default)]
    secrets: Option<SecretsSection>,
    #[serde(default)]
    exec: Option<HashMap<String, ExecEntryRaw>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpSection {
    #[serde(default)]
    servers: HashMap<String, McpServerEntryRaw>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct McpServerEntryRaw {
    url: VarTemplate,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    max_disconnected_time: Option<u64>,
    #[serde(default)]
    initial_retry_interval: Option<u64>,
    #[serde(default)]
    override_protocol_version: Option<String>,
    #[serde(default)]
    headers: Option<HashMap<String, VarTemplate>>,
    #[serde(default)]
    vars: Option<HashMap<String, VarSource>>,
    #[serde(default)]
    secrets: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ExecEntryRaw {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    /// Inherited variables to drop before `env` is applied, for a name the
    /// launched command must not see even though this process has it.
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    env: HashMap<String, VarSource>,
}

/// Resolved server profile for MCP `proxy`.
#[derive(Debug, Clone)]
pub struct ResolvedMcpServer {
    /// The `[secrets.backends]` entry this server addresses, if it names one.
    pub secrets: Option<String>,
    pub url: SecretString,
    pub transport: Option<String>,
    pub max_disconnected_time: Option<u64>,
    pub initial_retry_interval: Option<u64>,
    pub override_protocol_version: Option<String>,
    pub http_headers: HashMap<HeaderName, SecretString>,
}

pub fn trg_config_path() -> PathBuf {
    env_config_dir().join("trg").join("config.toml")
}

fn env_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map_or_else(|| PathBuf::from("/").join(".config"), |home| home.join(".config"))
        })
}

/// Everything one `trg mcp` invocation needs out of the config file.
///
/// Loaded in a single read so that `[mcp]` and `[secrets]` cannot drift apart
/// between two parses of the same file.
#[derive(Debug)]
pub struct LoadedMcp {
    pub server: ResolvedMcpServer,
    pub secrets: SecretsSection,
}

/// A config that has been read and validated as far as it can be without
/// talking to anything.
///
/// The file is parsed, the server is selected, and its declarations are known.
/// What is left is the values behind any `{ backend = ... }` vars, which is a
/// network read and so is not something a config loader should be doing on its
/// own. The caller builds a [`crate::secrets::Registry`] out of
/// [`PendingMcp::secrets`], fetches those, and calls [`PendingMcp::finish`].
///
/// Nothing before that point needs a reachable backend, so a config with a
/// syntax error is still reported as a syntax error on a machine that is
/// offline or holding an expired token.
#[derive(Debug)]
pub struct PendingMcp {
    raw: McpServerEntryRaw,
    pub secrets: SecretsSection,
}

impl PendingMcp {
    /// The `[secrets.backends]` entry this server names for its own OAuth
    /// credentials, readable before anything is fetched because it is a plain
    /// string rather than a var. Commands that only touch stored credentials
    /// need this and nothing else from the entry.
    pub fn server_secrets(&self) -> Option<&str> {
        self.raw.secrets.as_deref()
    }

    /// Every distinct secret this server's vars name.
    ///
    /// Deduplicated, because two vars pointing at the same coordinates are one
    /// read, and sorted so the order a caller sees does not depend on hashing.
    pub fn secret_vars(&self) -> Vec<SecretVar> {
        let Some(vars) = &self.raw.vars else {
            return Vec::new();
        };
        let mut out: Vec<SecretVar> = vars.values().filter_map(|v| v.secret().cloned()).collect();
        out.sort_by(|a, b| (&a.backend, &a.path, &a.key).cmp(&(&b.backend, &b.path, &b.key)));
        out.dedup();
        out
    }

    pub fn finish(self, fetched: &FetchedSecrets) -> Result<LoadedMcp, ConfigError> {
        finish_mcp(self, fetched)
    }
}

/// A launch target for `trg exec`, its `env` fully resolved.
#[derive(Debug, Clone)]
pub struct LoadedExec {
    pub command: String,
    pub args: Vec<String>,
    pub unset: Vec<String>,
    pub env: HashMap<String, String>,
}

/// An exec entry that has been read and validated, with its secret vars
/// still unread — the same split [`PendingMcp`] makes, for the same reason:
/// a config with a syntax error should read as one whether or not a backend
/// it names is reachable.
#[derive(Debug)]
pub struct PendingExec {
    raw: ExecEntryRaw,
    pub secrets: SecretsSection,
}

impl PendingExec {
    /// Every distinct secret this entry's `env` names, deduplicated and
    /// sorted for the same reason [`PendingMcp::secret_vars`] is.
    pub fn secret_vars(&self) -> Vec<SecretVar> {
        let mut out: Vec<SecretVar> = self.raw.env.values().filter_map(|v| v.secret().cloned()).collect();
        out.sort_by(|a, b| (&a.backend, &a.path, &a.key).cmp(&(&b.backend, &b.path, &b.key)));
        out.dedup();
        out
    }

    pub fn finish(self, fetched: &FetchedSecrets) -> Result<LoadedExec, ConfigError> {
        finish_exec(self, fetched)
    }
}

pub fn load_mcp(selected_name: &str) -> Result<PendingMcp, ConfigError> {
    load_mcp_at(&trg_config_path(), selected_name)
}

pub fn load_exec(name: &str) -> Result<PendingExec, ConfigError> {
    load_exec_at(&trg_config_path(), name)
}

/// The `[secrets]` section on its own.
///
/// `load_mcp` refuses a config without `[mcp.servers]`, which is the right
/// answer for a command that is about to talk to an MCP server and the wrong
/// one for a command that only inspects a backend. Declaring a backend before
/// declaring anything that uses it is an ordinary order to do things in.
pub fn load_secrets() -> Result<SecretsSection, ConfigError> {
    load_secrets_at(&trg_config_path())
}

fn load_secrets_at(path: &Path) -> Result<SecretsSection, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, &e))?;
    let root: FileRoot = toml::from_str(&text)?;
    Ok(root.secrets.unwrap_or_default())
}

/// A config that exists but cannot be read is a different problem from one that
/// was never written, and reporting the second for the first sends someone off
/// to create a file that is already there.
fn read_error(path: &Path, e: &std::io::Error) -> ConfigError {
    if e.kind() == std::io::ErrorKind::NotFound {
        return ConfigError::NotFound(path.to_path_buf());
    }
    ConfigError::Unreadable {
        path: path.to_path_buf(),
        cause: e.to_string(),
    }
}

fn load_mcp_at(path: &Path, selected_name: &str) -> Result<PendingMcp, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, &e))?;

    let root: FileRoot = toml::from_str(&text)?;
    let secrets = root.secrets.unwrap_or_default();
    let mut servers = root
        .mcp
        .and_then(|m| (!m.servers.is_empty()).then_some(m.servers))
        .ok_or(ConfigError::NoMcpServers)?;

    let Some(raw) = servers.remove(selected_name) else {
        let names: Vec<_> = servers.keys().cloned().collect();
        return Err(ConfigError::UnknownServer {
            name: selected_name.to_owned(),
            available: names.join(", "),
        });
    };

    Ok(PendingMcp { raw, secrets })
}

fn load_exec_at(path: &Path, name: &str) -> Result<PendingExec, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, &e))?;

    let root: FileRoot = toml::from_str(&text)?;
    let secrets = root.secrets.unwrap_or_default();
    let mut entries = root
        .exec
        .and_then(|e| (!e.is_empty()).then_some(e))
        .ok_or(ConfigError::NoExecEntries)?;

    let Some(raw) = entries.remove(name) else {
        let names: Vec<_> = entries.keys().cloned().collect();
        return Err(ConfigError::UnknownExecEntry {
            name: name.to_owned(),
            available: names.join(", "),
        });
    };

    Ok(PendingExec { raw, secrets })
}

fn finish_exec(pending: PendingExec, fetched: &FetchedSecrets) -> Result<LoadedExec, ConfigError> {
    let PendingExec { raw, secrets: _ } = pending;

    let mut env = HashMap::with_capacity(raw.env.len());
    for (k, v) in &raw.env {
        env.insert(k.clone(), v.resolve(fetched)?);
    }

    Ok(LoadedExec {
        command: raw.command,
        args: raw.args,
        unset: raw.unset,
        env,
    })
}

fn finish_mcp(pending: PendingMcp, fetched: &FetchedSecrets) -> Result<LoadedMcp, ConfigError> {
    let PendingMcp { raw, secrets } = pending;

    let resolved_vars: HashMap<String, String> = match &raw.vars {
        None => HashMap::new(),
        Some(table) => {
            let mut m = HashMap::with_capacity(table.len());
            for (k, v) in table {
                m.insert(k.clone(), v.resolve(fetched)?);
            }
            m
        }
    };

    let url_string = raw.url.resolve(&resolved_vars)?;
    if url_string.trim().is_empty() {
        return Err(ConfigError::EmptyUrl);
    }

    let mut http_headers = HashMap::new();
    let mut header_origins: HashMap<HeaderName, String> = HashMap::new();
    if let Some(ref hdr_map) = raw.headers {
        for (k, vt) in hdr_map {
            let s = vt.resolve(&resolved_vars)?;
            if s.trim().is_empty() {
                return Err(ConfigError::InvalidHeaderValue {
                    name: k.clone(),
                    cause: "empty or whitespace-only values are not allowed".into(),
                });
            }
            let name = HeaderName::try_from(k.as_str())
                .map_err(|e| ConfigError::InvalidHeaderName(k.clone(), e.to_string()))?;
            http::HeaderValue::from_str(&s).map_err(|e| ConfigError::InvalidHeaderValue {
                name: k.clone(),
                cause: e.to_string(),
            })?;
            if let Some(existing) = header_origins.get(&name) {
                return Err(ConfigError::DuplicateHeader {
                    name: k.clone(),
                    existing: existing.clone(),
                });
            }
            header_origins.insert(name.clone(), k.clone());
            http_headers.insert(name, SecretString::new(s.into_boxed_str()));
        }
    }

    Ok(LoadedMcp {
        server: ResolvedMcpServer {
            secrets: raw.secrets,
            url: SecretString::new(url_string.into_boxed_str()),
            transport: raw.transport,
            max_disconnected_time: raw.max_disconnected_time,
            initial_retry_interval: raw.initial_retry_interval,
            override_protocol_version: raw.override_protocol_version,
            http_headers,
        },
        secrets,
    })
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use tempfile::tempdir;

    /// A config that exists but will not open must not be reported as one that
    /// was never written, since the two have opposite remedies.
    #[test]
    fn a_config_that_cannot_be_read_is_not_reported_as_missing() {
        let dir = tempdir().unwrap();

        let missing = read_error(
            &dir.path().join("never-written.toml"),
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(matches!(missing, ConfigError::NotFound(_)), "{missing}");

        // A directory opens and then fails to read, which is the shape of every
        // non-`NotFound` failure here.
        let unreadable = std::fs::read_to_string(dir.path()).expect_err("a directory is not readable");
        let err = read_error(dir.path(), &unreadable);
        assert!(matches!(err, ConfigError::Unreadable { .. }), "{err}");
        assert!(err.to_string().contains("could not be read"), "{err}");
    }

    static INTEG_TEST_ENV_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_integration_env(prefix: &'static str) -> String {
        format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            INTEG_TEST_ENV_SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn write_secure_config(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn load_at(path: &Path, server: &str) -> Result<ResolvedMcpServer, ConfigError> {
        load_full(path, server).map(|loaded| loaded.server)
    }

    fn load_full(path: &Path, server: &str) -> Result<LoadedMcp, ConfigError> {
        load_mcp_at(path, server)?.finish(&FetchedSecrets::new())
    }

    #[test]
    fn a_server_carries_the_backend_it_names_and_the_secrets_section_beside_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [secrets.backends.work]
            kind = "openbao"
            addr = "https://bao.example.com:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            machine_id = "laptop"
            token_file = "~/.vault-token"

            [mcp.servers.s1]
            url = "https://example.com/mcp"
            secrets = "work"

            [mcp.servers.s2]
            url = "https://example.com/mcp"
            "#,
        );

        let loaded = load_full(&path, "s1").unwrap();
        assert_eq!(loaded.server.secrets.as_deref(), Some("work"));
        assert!(loaded.secrets.backends.contains_key("work"));

        let loaded = load_full(&path, "s2").unwrap();
        assert_eq!(loaded.server.secrets, None);
        assert!(
            loaded.secrets.backends.contains_key("work"),
            "the section travels with every server, so `[mcp]` and `[secrets]` cannot drift"
        );
    }

    /// Parsing must not depend on a reachable backend: a config with a syntax
    /// error has to read as one on a machine that is offline or holding an
    /// expired token, which is exactly when someone is editing it.
    #[test]
    fn a_secret_var_is_declared_at_parse_time_and_read_later() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [mcp.servers.s1]
            url = "https://example.com/mcp"

            [mcp.servers.s1.headers]
            Authorization = { var = "token" }

            [mcp.servers.s1.vars]
            token = { backend = "homelab", path = "agentgateway", key = "token" }
            "#,
        );

        let pending = load_mcp_at(&path, "s1").expect("parses without reaching anything");
        assert_eq!(
            pending.secret_vars(),
            vec![SecretVar {
                backend: "homelab".into(),
                path: "agentgateway".into(),
                key: "token".into(),
            }]
        );

        let mut fetched = FetchedSecrets::new();
        fetched.insert(pending.secret_vars()[0].clone(), SecretString::from("t".to_string()));

        let loaded = pending.finish(&fetched).unwrap();
        assert_eq!(
            loaded.server.http_headers[&HeaderName::from_static("authorization")].expose_secret(),
            "t"
        );
    }

    #[test]
    fn one_entry_read_by_two_vars_is_named_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [mcp.servers.s1]
            url = "https://example.com/mcp"

            [mcp.servers.s1.vars]
            a = { backend = "homelab", path = "agentgateway", key = "token" }
            b = { backend = "homelab", path = "agentgateway", key = "token" }
            c = { backend = "homelab", path = "agentgateway", key = "principal" }
            d = "literal"
            "#,
        );

        let wanted = load_mcp_at(&path, "s1").unwrap().secret_vars();

        assert_eq!(
            wanted.len(),
            2,
            "the duplicate collapses and the literal is not a backend read: {wanted:?}"
        );
        assert_eq!(
            wanted[0].key, "principal",
            "sorted, so the order does not depend on hashing"
        );
        assert_eq!(wanted[1].key, "token");
    }

    #[test]
    fn a_server_declaring_no_secret_vars_wants_nothing_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [mcp.servers.s1]
            url = "https://example.com/mcp"

            [mcp.servers.s1.vars]
            a = "literal"
            "#,
        );

        assert!(load_mcp_at(&path, "s1").unwrap().secret_vars().is_empty());
    }

    #[test]
    fn a_config_without_a_secrets_section_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [mcp.servers.s1]
            url = "https://example.com/mcp"
            "#,
        );

        let loaded = load_mcp_at(&path, "s1").unwrap();
        assert!(loaded.secrets.backends.is_empty());
    }

    #[test]
    fn load_rejects_empty_url() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.blank]
url = ""
"#,
        );
        assert!(matches!(load_at(&path, "blank").unwrap_err(), ConfigError::EmptyUrl));
    }

    #[test]
    fn load_rejects_whitespace_only_url() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.blank]
url = "   "
"#,
        );
        assert!(matches!(load_at(&path, "blank").unwrap_err(), ConfigError::EmptyUrl));
    }

    #[test]
    fn load_rejects_duplicate_header_after_canonicalization() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
Authorization = "Bearer one"
authorization = "Bearer two"
"#,
        );
        let e = load_at(&path, "u").unwrap_err();
        let ConfigError::DuplicateHeader { ref name, ref existing } = e else {
            panic!("expected DuplicateHeader, got {e:?}");
        };
        let pair = [name.as_str(), existing.as_str()];
        assert!(
            pair.contains(&"Authorization") && pair.contains(&"authorization"),
            "{pair:?}"
        );
    }

    #[test]
    fn load_rejects_empty_resolved_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
X = ""
"#,
        );
        let e = load_at(&path, "u").unwrap_err();
        assert!(
            matches!(
                e,
                ConfigError::InvalidHeaderValue {
                    ref name,
                    ref cause,
                } if name == "X" && cause.contains("empty")
            ),
            "{e:?}"
        );
    }

    #[test]
    fn load_rejects_whitespace_only_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
X = "   "
"#,
        );
        let e = load_at(&path, "u").unwrap_err();
        assert!(
            matches!(e, ConfigError::InvalidHeaderValue { ref name, .. } if name == "X"),
            "{e:?}"
        );
    }

    #[test]
    fn load_rejects_empty_url_when_env_default_empty() {
        let key = unique_integration_env("EMPTY_URL");
        std::env::remove_var(&key);
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.u]
url = {{ var = "u" }}

[mcp.servers.u.vars]
u = {{ env = "{key}", default = "" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        assert!(matches!(load_at(&path, "u").unwrap_err(), ConfigError::EmptyUrl));
    }

    #[test]
    fn load_resolves_literal_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.alpha]
url = "https://example.com/mcp"

[mcp.servers.alpha.headers]
Authorization = "Bearer secret123"
"#,
        );
        let r = load_at(&path, "alpha").unwrap();
        let auth = http::HeaderName::from_static("authorization");
        assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer secret123");
    }

    #[test]
    fn load_header_from_env_var_source() {
        let key = unique_integration_env("TRG_TOK_ENV");
        std::env::set_var(&key, "Bearer from-environment");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.e]
url = "https://e.example/mcp"

[mcp.servers.e.vars]
auth = {{ env = "{key}" }}

[mcp.servers.e.headers]
Authorization = {{ var = "auth" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "e").unwrap();
        let auth = http::HeaderName::from_static("authorization");
        assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer from-environment");
        std::env::remove_var(&key);
    }

    #[test]
    fn load_header_env_uses_default_when_unset() {
        let key = unique_integration_env("TRG_TOK_ABSENT");
        std::env::remove_var(&key);
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.d]
url = "https://d.example/mcp"

[mcp.servers.d.vars]
auth = {{ env = "{key}", default = "fallback-val" }}

[mcp.servers.d.headers]
Authorization = {{ var = "auth" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "d").unwrap();
        let auth = http::HeaderName::from_static("authorization");
        assert_eq!(r.http_headers[&auth].expose_secret(), "fallback-val");
    }

    #[test]
    fn load_fails_when_header_env_required_but_unset() {
        let key = unique_integration_env("TRG_TOK_REQUIRED");
        std::env::remove_var(&key);
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.x]
url = "https://x.example/mcp"

[mcp.servers.x.vars]
auth = {{ env = "{key}" }}

[mcp.servers.x.headers]
Authorization = {{ var = "auth" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        let e = load_at(&path, "x").unwrap_err();
        assert!(matches!(e, ConfigError::VarResolve(_)), "{e:?}");
    }

    #[test]
    fn unknown_server_lists_available_names() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.zebra]
url = "https://z"

[mcp.servers.alpha]
url = "https://a"
"#,
        );
        let err = load_at(&path, "missing").unwrap_err();
        let ConfigError::UnknownServer {
            ref name,
            ref available,
        } = err
        else {
            panic!("unexpected err: {err:?}");
        };
        assert_eq!(name, "missing");
        let listed: HashSet<&str> = available.split(", ").collect();
        assert_eq!(listed, HashSet::from(["alpha", "zebra"]));
    }

    #[test]
    fn load_rejects_unknown_root_toml_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(&path, "key = 1\n");
        let err = load_at(&path, "x").unwrap_err();
        assert!(matches!(err, ConfigError::Toml(_)), "{err:?}");
    }

    #[test]
    fn load_without_mcp_table_yields_no_mcp_servers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(&path, "");
        let err = load_at(&path, "x").unwrap_err();
        assert!(matches!(err, ConfigError::NoMcpServers), "{err:?}");
    }

    #[test]
    fn load_rejects_empty_servers_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp]
"#,
        );
        let err = load_at(&path, "x").unwrap_err();
        assert!(matches!(err, ConfigError::NoMcpServers));
    }

    #[test]
    fn load_accepts_dollar_sequence_in_literal_url() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.s1]
url = "https://bad${x}"
"#,
        );
        let r = load_at(&path, "s1").unwrap();
        assert_eq!(r.url.expose_secret(), "https://bad${x}");
    }

    #[test]
    fn load_accepts_dollar_sequence_in_resolved_env_url() {
        let key = unique_integration_env("TRG_BAD_URL");
        std::env::set_var(&key, "https://bad${oops}");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.badurl]
url = {{ var = "u" }}

[mcp.servers.badurl.vars]
u = {{ env = "{key}" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "badurl").unwrap();
        assert_eq!(r.url.expose_secret(), "https://bad${oops}");
        std::env::remove_var(&key);
    }

    #[test]
    fn load_url_from_env_var_source() {
        let key = unique_integration_env("TRG_MCP_URL");
        std::env::set_var(&key, "https://from-env.example/mcp");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.uenv]
url = {{ var = "u" }}

[mcp.servers.uenv.vars]
u = {{ env = "{key}" }}
"#,
            key = key,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "uenv").unwrap();
        assert_eq!(r.url.expose_secret(), "https://from-env.example/mcp");
        std::env::remove_var(&key);
    }

    #[test]
    fn load_url_literal_without_placeholders_loads_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.ui]
url = "https://svc.example.org/mcp"
"#,
        );
        let r = load_at(&path, "ui").unwrap();
        assert_eq!(r.url.expose_secret(), "https://svc.example.org/mcp");
    }

    #[test]
    fn load_accepts_dollar_sequence_in_plain_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
Authorization = "Bearer ${wrong}"
"#,
        );
        let r = load_at(&path, "u").unwrap();
        let auth = http::HeaderName::from_static("authorization");
        assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer ${wrong}");
    }

    #[test]
    fn load_rejects_unknown_server_field() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.s1]
url = "https://ok"
resource = "oops"
"#,
        );
        assert!(matches!(load_at(&path, "s1").unwrap_err(), ConfigError::Toml(_)));
    }

    #[test]
    fn load_accepts_vars_table_with_literal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.v]
url = { var = "endpoint" }

[mcp.servers.v.vars]
endpoint = "https://lit.example/x"
"#,
        );
        let r = load_at(&path, "v").unwrap();
        assert_eq!(r.url.expose_secret(), "https://lit.example/x");
    }

    #[test]
    fn load_rejects_inline_env_in_url() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.s]
url = { env = "ANYTHING" }
"#,
        );
        let err = load_at(&path, "s").unwrap_err();
        assert!(matches!(err, ConfigError::Toml(_)), "{err:?}");
    }

    #[test]
    fn load_rejects_undefined_var_reference() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.s]
url = { var = "missing" }
"#,
        );
        let err = load_at(&path, "s").unwrap_err();
        assert!(
            matches!(&err, ConfigError::VarResolve(VarResolveError::UndefinedVar(n)) if n == "missing"),
            "{err:?}"
        );
    }

    #[test]
    fn transport_comes_from_toml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
[mcp.servers.s1]
url = "https://ok"
transport = "from-toml"
"#,
        );
        let resolved = load_at(&path, "s1").unwrap();
        assert_eq!(resolved.transport.as_deref(), Some("from-toml"));
    }

    #[test]
    fn load_missing_config_yields_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.toml");
        assert!(matches!(
            load_mcp_at(&path, "s1").unwrap_err(),
            ConfigError::NotFound(_)
        ));
    }

    #[test]
    fn header_value_parses_plain_varref_and_segments() {
        #[derive(Debug, Deserialize)]
        struct H {
            headers: HashMap<String, VarTemplate>,
        }
        let t = r#"
[headers]
A = "plain"
B = { var = "tok" }
C = ["x-", { var = "tok" }, "-z"]
"#;
        let h: H = toml::from_str(t).unwrap();
        match &h.headers["A"] {
            VarTemplate::Single(Segment::Literal(s)) => assert_eq!(s, "plain"),
            _ => panic!("expected Single(Literal)"),
        }
        match &h.headers["B"] {
            VarTemplate::Single(Segment::Ref(VarRef { var })) => assert_eq!(var, "tok"),
            _ => panic!("expected Single(Ref)"),
        }
        match &h.headers["C"] {
            VarTemplate::Segments(segs) => assert_eq!(segs.len(), 3),
            _ => panic!("expected Segments"),
        }
    }

    #[test]
    fn load_url_from_segments() {
        let host = unique_integration_env("TRG_SEG_URL_HOST");
        std::env::set_var(&host, "mcp.example.org");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.composed]
url = [
    "https://",
    {{ var = "host" }},
    "/v1/stream",
]

[mcp.servers.composed.vars]
host = {{ env = "{host}" }}
"#,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "composed").unwrap();
        assert_eq!(r.url.expose_secret(), "https://mcp.example.org/v1/stream");
        std::env::remove_var(&host);
    }

    #[test]
    fn load_header_from_segments() {
        let token = unique_integration_env("TRG_SEG_TOKEN");
        std::env::set_var(&token, "abc123");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.h]
url = "https://ok"

[mcp.servers.h.vars]
token = {{ env = "{token}" }}

[mcp.servers.h.headers]
Authorization = ["Bearer ", {{ var = "token" }}]
"#,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "h").unwrap();
        let auth = http::HeaderName::from_static("authorization");
        assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer abc123");
        std::env::remove_var(&token);
    }

    #[test]
    fn load_segment_propagates_missing_env() {
        let missing = unique_integration_env("TRG_SEG_REQ");
        std::env::remove_var(&missing);
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.s]
url = ["https://", {{ var = "host" }}, "/x"]

[mcp.servers.s.vars]
host = {{ env = "{missing}" }}
"#,
        );
        write_secure_config(&path, &cfg);
        let err = load_at(&path, "s").unwrap_err();
        assert!(
            matches!(&err, ConfigError::VarResolve(VarResolveError::MissingEnv(n)) if n == &missing),
            "{err:?}"
        );
    }

    #[test]
    fn load_vars_table_supports_literal_and_env() {
        let host = unique_integration_env("TRG_VARS_HOST");
        std::env::set_var(&host, "h.example");
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = format!(
            r#"
[mcp.servers.both]
url = ["https://", {{ var = "host" }}, "/", {{ var = "api" }}, "/stream"]

[mcp.servers.both.vars]
host = {{ env = "{host}" }}
api  = "v1"
"#,
        );
        write_secure_config(&path, &cfg);
        let r = load_at(&path, "both").unwrap();
        assert_eq!(r.url.expose_secret(), "https://h.example/v1/stream");
        std::env::remove_var(&host);
    }

    fn load_exec_full(path: &Path, name: &str) -> Result<LoadedExec, ConfigError> {
        load_exec_at(path, name)?.finish(&FetchedSecrets::new())
    }

    #[test]
    fn a_exec_entry_carries_its_command_args_and_secrets_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [secrets.backends.work]
            kind = "openbao"
            addr = "https://bao.example.com:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token = "irrelevant"

            [exec.claude]
            command = "claude"
            args = ["--dangerously-skip-permissions"]
            "#,
        );

        let loaded = load_exec_full(&path, "claude").unwrap();
        assert_eq!(loaded.command, "claude");
        assert_eq!(loaded.args, vec!["--dangerously-skip-permissions".to_string()]);
        assert!(loaded.env.is_empty());
        assert!(loaded.unset.is_empty());

        let pending = load_exec_at(&path, "claude").unwrap();
        assert!(
            pending.secrets.backends.contains_key("work"),
            "the section travels with every entry, same as it does for mcp servers"
        );
    }

    #[test]
    fn an_unknown_exec_entry_lists_the_ones_that_are_declared() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [exec.zebra]
            command = "z"

            [exec.alpha]
            command = "a"
            "#,
        );

        let err = load_exec_at(&path, "missing").unwrap_err();
        let ConfigError::UnknownExecEntry { name, available } = err else {
            panic!("expected UnknownExecEntry, got {err:?}");
        };
        assert_eq!(name, "missing");
        let listed: HashSet<&str> = available.split(", ").collect();
        assert_eq!(listed, HashSet::from(["alpha", "zebra"]));
    }

    #[test]
    fn a_config_without_an_exec_table_yields_no_exec_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(&path, "");
        assert!(matches!(
            load_exec_at(&path, "x").unwrap_err(),
            ConfigError::NoExecEntries
        ));
    }

    #[test]
    fn a_exec_entry_resolves_secret_and_literal_env_and_dedups_the_vars_it_wants() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_secure_config(
            &path,
            r#"
            [exec.p]
            command = "claude"
            unset = ["ANTHROPIC_API_KEY"]

            [exec.p.env]
            MODE = "prod"
            TOKEN = { backend = "homelab", path = "agentgateway", key = "token" }
            TOKEN_AGAIN = { backend = "homelab", path = "agentgateway", key = "token" }
            "#,
        );

        let pending = load_exec_at(&path, "p").unwrap();
        let wanted = pending.secret_vars();
        assert_eq!(wanted.len(), 1, "two vars at the same address are one read: {wanted:?}");

        let mut fetched = FetchedSecrets::new();
        fetched.insert(wanted[0].clone(), SecretString::from("t".to_string()));

        let loaded = pending.finish(&fetched).unwrap();
        assert_eq!(loaded.unset, vec!["ANTHROPIC_API_KEY".to_string()]);
        assert_eq!(loaded.env["MODE"], "prod");
        assert_eq!(loaded.env["TOKEN"], "t");
        assert_eq!(loaded.env["TOKEN_AGAIN"], "t");
    }
}
