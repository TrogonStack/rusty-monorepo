//! `~/.config/trg/config.toml` loader for `trg mcp *` (and future subcommands).
//!
//! Env-backed inputs are declared once in `[mcp.servers.<name>.vars]` as
//! `VarSource` entries (literal string, `{ env, default? }` table, or a table
//! naming a secrets backend and addressing one value inside it in that
//! backend's own vocabulary, `{ backend, path, key }` or
//! `{ backend, ref }`). The
//! server's `url` and each header value (`VarTemplate`) accept three shapes:
//!   - a TOML string (literal),
//!   - a `{ var = "<name>" }` reference to a `vars` entry,
//!   - a TOML array mixing the above two, concatenated in order.
//!
//! Inline `{ env = "..." }` is rejected outside the `vars` table.
//!
//! `[exec.<name>.env]` has no separate `vars` table to indirect through, so
//! each value (`EnvValue`) accepts a `VarSource` directly, or a TOML array of
//! them concatenated in order. Inline `{ env = "..." }` and a secret table are
//! both allowed inside that array.

mod var;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use http::HeaderName;
pub use secrecy::SecretString;
use serde::Deserialize;
use tracing::field::Empty;
pub use var::{
    EnvValue, FetchedSecrets, RawEnvValue, RawSecretVar, RawVarSource, SecretRead, SecretVar, SecretVarError, Segment,
    VarRef, VarResolveError, VarSite, VarSource, VarTemplate, WithFallbacks,
};

use crate::secrets::{MirrorConfigError, SecretsSection};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Toml(#[from] toml::de::Error),

    #[error("config file not found at `{0}`")]
    NotFound(PathBuf),

    #[error("config file at `{path}` could not be read: {cause}")]
    Unreadable {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("no `[mcp.servers]` section in config")]
    NoMcpServers,

    #[error("unknown MCP server `{name}` — known: {available}")]
    UnknownServer { name: String, available: String },

    #[error("could not decode header `{name}`: {cause}")]
    InvalidHeaderValue {
        name: String,
        #[source]
        cause: HeaderValueError,
    },

    #[error("MCP server `url` must not be empty")]
    EmptyUrl,

    #[error("variable resolution failed: {0}")]
    VarResolve(#[from] VarResolveError),

    #[error("invalid header name `{0}`: {1}")]
    InvalidHeaderName(String, #[source] http::header::InvalidHeaderName),

    #[error("duplicate header `{name}` collides with `{existing}` after canonicalization")]
    DuplicateHeader { name: String, existing: String },

    #[error("no `[exec]` entries in config")]
    NoExecEntries,

    #[error("unknown exec entry `{name}` — known: {available}")]
    UnknownExecEntry { name: String, available: String },

    #[error("{0}")]
    SecretVar(#[from] SecretVarError),

    #[error("{0}")]
    Mirror(#[from] MirrorConfigError),
}

#[derive(Debug, thiserror::Error)]
pub enum HeaderValueError {
    #[error("empty or whitespace-only values are not allowed")]
    Empty,
    #[error("{0}")]
    Invalid(#[source] http::header::InvalidHeaderValue),
}

impl ConfigError {
    /// A low-cardinality name for this failure, for `error.type`.
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Toml(_) => "toml",
            Self::NotFound(_) => "not_found",
            Self::Unreadable { .. } => "unreadable",
            Self::NoMcpServers => "no_mcp_servers",
            Self::UnknownServer { .. } => "unknown_server",
            Self::InvalidHeaderValue { .. } => "invalid_header_value",
            Self::EmptyUrl => "empty_url",
            Self::VarResolve(_) => "var_resolve",
            Self::InvalidHeaderName(..) => "invalid_header_name",
            Self::DuplicateHeader { .. } => "duplicate_header",
            Self::NoExecEntries => "no_exec_entries",
            Self::UnknownExecEntry { .. } => "unknown_exec_entry",
            Self::SecretVar(_) => "secret_var",
            Self::Mirror(_) => "mirror",
        }
    }
}

/// Reads, parses and validates the config under one span, since that is the
/// whole of what loading it costs. The file path is not recorded: it sits
/// under the operator's home directory.
fn traced_load<T>(load: impl FnOnce() -> Result<T, ConfigError>) -> Result<T, ConfigError> {
    let span = tracing::info_span!("load config", "error.type" = Empty, "otel.status_code" = Empty);
    let result = span.in_scope(load);
    if let Err(e) = &result {
        crate::secrets::telemetry::record_error(&span, e.error_type());
    }
    result
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

/// One `[mcp.servers.<name>]` entry.
///
/// Generic over the secret payload so that the entry read out of the file and
/// the entry whose vars have been checked against the declared backends are
/// different types. Deserialisation only ever produces the raw one.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, bound(deserialize = "S: Deserialize<'de>"))]
struct McpServerEntry<S = SecretVar> {
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
    vars: Option<HashMap<String, VarSource<S>>>,
    #[serde(default)]
    secrets: Option<String>,
}

type McpServerEntryRaw = McpServerEntry<RawSecretVar>;

impl McpServerEntryRaw {
    fn into_resolved(self, name: &str, secrets: &SecretsSection) -> Result<McpServerEntry, SecretVarError> {
        let vars = match self.vars {
            None => None,
            Some(table) => Some(
                table
                    .into_iter()
                    .map(|(key, source)| {
                        source
                            .into_resolved(secrets, || VarSite::mcp_var(name, &key))
                            .map(|source| (key, source))
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?,
            ),
        };
        Ok(McpServerEntry {
            url: self.url,
            transport: self.transport,
            max_disconnected_time: self.max_disconnected_time,
            initial_retry_interval: self.initial_retry_interval,
            override_protocol_version: self.override_protocol_version,
            headers: self.headers,
            vars,
            secrets: self.secrets,
        })
    }
}

/// One `[exec.<name>]` entry, generic for the reason [`McpServerEntry`] is.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, bound(deserialize = "S: Deserialize<'de>"))]
struct ExecEntry<S = SecretVar> {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    /// Inherited variables to drop before `env` is applied, for a name the
    /// launched command must not see even though this process has it.
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    env: HashMap<String, EnvValue<S>>,
}

type ExecEntryRaw = ExecEntry<RawSecretVar>;

impl ExecEntryRaw {
    fn into_resolved(self, name: &str, secrets: &SecretsSection) -> Result<ExecEntry, SecretVarError> {
        let env = self
            .env
            .into_iter()
            .map(|(key, value)| {
                value
                    .into_resolved(secrets, || VarSite::exec_env(name, &key))
                    .map(|value| (key, value))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(ExecEntry {
            command: self.command,
            args: self.args,
            unset: self.unset,
            env,
        })
    }
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
    trg_config_dir().join("config.toml")
}

pub fn trg_config_dir() -> PathBuf {
    env_config_dir().join("trg")
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
    entry: McpServerEntry,
    pub secrets: SecretsSection,
}

impl PendingMcp {
    /// The `[secrets.backends]` entry this server names for its own OAuth
    /// credentials, readable before anything is fetched because it is a plain
    /// string rather than a var. Commands that only touch stored credentials
    /// need this and nothing else from the entry.
    pub fn server_secrets(&self) -> Option<&str> {
        self.entry.secrets.as_deref()
    }

    /// Every distinct secret this server's vars name.
    ///
    /// Deduplicated, because two vars pointing at the same coordinates are one
    /// read, and sorted so the order a caller sees does not depend on hashing.
    pub fn secret_vars(&self) -> Vec<SecretRead> {
        let Some(vars) = &self.entry.vars else {
            return Vec::new();
        };
        sorted_distinct(vars.values().filter_map(VarSource::secret_read))
    }

    pub fn finish(self, fetched: &FetchedSecrets) -> Result<LoadedMcp, ConfigError> {
        self.finish_with_env(fetched, &|name| std::env::var(name).ok())
    }

    fn finish_with_env(
        self,
        fetched: &FetchedSecrets,
        environment: &impl Fn(&str) -> Option<String>,
    ) -> Result<LoadedMcp, ConfigError> {
        finish_mcp(self, fetched, environment)
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
    entry: ExecEntry,
    pub secrets: SecretsSection,
}

impl PendingExec {
    /// Every distinct secret this entry's `env` names, deduplicated and
    /// sorted for the same reason [`PendingMcp::secret_vars`] is.
    pub fn secret_vars(&self) -> Vec<SecretRead> {
        sorted_distinct(self.entry.env.values().flat_map(EnvValue::secret_reads))
    }

    pub fn finish(self, fetched: &FetchedSecrets) -> Result<LoadedExec, ConfigError> {
        self.finish_with_env(fetched, &|name| std::env::var(name).ok())
    }

    fn finish_with_env(
        self,
        fetched: &FetchedSecrets,
        environment: &impl Fn(&str) -> Option<String>,
    ) -> Result<LoadedExec, ConfigError> {
        finish_exec(self, fetched, environment)
    }
}

/// Deduplicated and ordered, because two vars pointing at the same address
/// are one read, and the order a caller sees should not depend on hashing.
///
/// Ordered on the rendered form rather than on the address itself: an address
/// is not comparable across backend kinds, and what this is for is a stable
/// sequence rather than a meaningful one.
fn sorted_distinct(vars: impl Iterator<Item = SecretRead>) -> Vec<SecretRead> {
    let mut out: Vec<SecretRead> = vars.collect();
    out.sort_by_cached_key(|v| (v.primary().backend().to_string(), v.to_string()));
    out.dedup();
    out
}

pub fn load_mcp(selected_name: &str) -> Result<PendingMcp, ConfigError> {
    traced_load(|| load_mcp_at(&trg_config_path(), selected_name))
}

pub fn load_exec(name: &str) -> Result<PendingExec, ConfigError> {
    traced_load(|| load_exec_at(&trg_config_path(), name))
}

/// Every `[exec.<name>]` name declared in the config, sorted.
///
/// An empty or missing `[exec]` table is a plain empty list here rather than
/// [`ConfigError::NoExecEntries`]: that error exists to stop a launch that has
/// nothing to launch, and listing a config that declares nothing yet is not
/// that — it is the ordinary state of a config nobody has written an entry
/// into.
pub fn list_exec_names() -> Result<Vec<String>, ConfigError> {
    traced_load(|| list_exec_names_at(&trg_config_path()))
}

fn list_exec_names_at(path: &Path) -> Result<Vec<String>, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, e))?;
    let root: FileRoot = toml::from_str(&text)?;
    let mut names: Vec<String> = root.exec.unwrap_or_default().into_keys().collect();
    names.sort();
    Ok(names)
}

/// The `[secrets]` section on its own.
///
/// `load_mcp` refuses a config without `[mcp.servers]`, which is the right
/// answer for a command that is about to talk to an MCP server and the wrong
/// one for a command that only inspects a backend. Declaring a backend before
/// declaring anything that uses it is an ordinary order to do things in.
pub fn load_secrets() -> Result<SecretsSection, ConfigError> {
    traced_load(|| load_secrets_at(&trg_config_path()))
}

fn load_secrets_at(path: &Path) -> Result<SecretsSection, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, e))?;
    let root: FileRoot = toml::from_str(&text)?;
    let secrets = root.secrets.unwrap_or_default();
    secrets.resolve_mirrors()?;
    secrets.check_named()?;
    Ok(secrets)
}

/// A config that exists but cannot be read is a different problem from one that
/// was never written, and reporting the second for the first sends someone off
/// to create a file that is already there.
fn read_error(path: &Path, e: std::io::Error) -> ConfigError {
    if e.kind() == std::io::ErrorKind::NotFound {
        return ConfigError::NotFound(path.to_path_buf());
    }
    ConfigError::Unreadable {
        path: path.to_path_buf(),
        cause: e,
    }
}

/// Check every var in the rest of the document, so a mistyped address is a
/// config error wherever it was written.
///
/// A var is validated against the kind of the backend it names, and that
/// pairing is a property of the file rather than of the entry being launched.
/// Reporting it only for the selected entry would mean a config that loads
/// today and fails tomorrow because a different server was asked for, which
/// is the failure mode this whole phase exists to remove.
fn check_mcp_addresses(mcp: Option<McpSection>, secrets: &SecretsSection) -> Result<(), SecretVarError> {
    for (name, entry) in mcp.map(|m| m.servers).unwrap_or_default() {
        entry.into_resolved(&name, secrets)?;
    }
    Ok(())
}

/// See [`check_mcp_addresses`].
fn check_exec_addresses(
    exec: Option<HashMap<String, ExecEntryRaw>>,
    secrets: &SecretsSection,
) -> Result<(), SecretVarError> {
    for (name, entry) in exec.unwrap_or_default() {
        entry.into_resolved(&name, secrets)?;
    }
    Ok(())
}

fn load_mcp_at(path: &Path, selected_name: &str) -> Result<PendingMcp, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, e))?;

    let root: FileRoot = toml::from_str(&text)?;
    let secrets = root.secrets.unwrap_or_default();
    secrets.resolve_mirrors()?;
    secrets.check_named()?;
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

    let entry = raw.into_resolved(selected_name, &secrets)?;
    check_exec_addresses(root.exec, &secrets)?;
    for (name, other) in servers {
        other.into_resolved(&name, &secrets)?;
    }

    Ok(PendingMcp { entry, secrets })
}

fn load_exec_at(path: &Path, name: &str) -> Result<PendingExec, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| read_error(path, e))?;

    let root: FileRoot = toml::from_str(&text)?;
    let secrets = root.secrets.unwrap_or_default();
    secrets.resolve_mirrors()?;
    secrets.check_named()?;
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

    let entry = raw.into_resolved(name, &secrets)?;
    check_mcp_addresses(root.mcp, &secrets)?;
    for (name, other) in entries {
        other.into_resolved(&name, &secrets)?;
    }

    Ok(PendingExec { entry, secrets })
}

fn finish_exec(
    pending: PendingExec,
    fetched: &FetchedSecrets,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<LoadedExec, ConfigError> {
    let PendingExec { entry, secrets: _ } = pending;

    let mut env = HashMap::with_capacity(entry.env.len());
    for (k, v) in &entry.env {
        env.insert(k.clone(), v.resolve_with_env(fetched, environment)?);
    }

    Ok(LoadedExec {
        command: entry.command,
        args: entry.args,
        unset: entry.unset,
        env,
    })
}

fn finish_mcp(
    pending: PendingMcp,
    fetched: &FetchedSecrets,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<LoadedMcp, ConfigError> {
    let PendingMcp { entry: raw, secrets } = pending;

    let resolved_vars: HashMap<String, String> = match &raw.vars {
        None => HashMap::new(),
        Some(table) => {
            let mut m = HashMap::with_capacity(table.len());
            for (k, v) in table {
                m.insert(k.clone(), v.resolve_with_env(fetched, environment)?);
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
                    cause: HeaderValueError::Empty,
                });
            }
            let name = HeaderName::try_from(k.as_str()).map_err(|e| ConfigError::InvalidHeaderName(k.clone(), e))?;
            http::HeaderValue::from_str(&s).map_err(|e| ConfigError::InvalidHeaderValue {
                name: k.clone(),
                cause: HeaderValueError::Invalid(e),
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
mod tests;
