//! OpenBao KV v2 backend, spoken over the HTTP API directly.
//!
//! # Why raw `reqwest` rather than a client crate
//!
//! The surface used here is five endpoints. A client crate would add a
//! dependency tree, its own error taxonomy to translate into
//! [`SecretsError`], and its own retry and renewal policies, all of which this
//! design deliberately does not want.
//!
//! # The status codes this maps, and how they were established
//!
//! Verified against OpenBao v2.6.0, because the inherited Vault documentation
//! does not state several of them:
//!
//! - A missing secret is `404` with an **empty** `errors` array.
//! - A missing *mount* is also `404`, but with a non-empty `errors` array. That
//!   is a configuration error, not a miss, so the two are distinguished by the
//!   body rather than the status.
//! - A rejected token is `403`, not `401`.
//! - `DELETE .../metadata/...` answers `204` whether or not the secret existed.
//! - `LIST` on an empty or absent folder is `404` with an empty `errors` array.
//!
//! # Why the token is re-read on every operation
//!
//! `trg mcp proxy` is a long-lived child process. Reading the token once at
//! construction would mean that a token expiring mid-session could only be
//! recovered by restarting the proxy, which in practice means restarting the
//! editor that spawned it. Re-reading makes `bao login` in any terminal enough.
//! The token is never persisted, cached, or renewed by `trg`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};

use super::kv_v2::{Envelope, ErrorBody, ListPayload, ReadPayload};
use super::{SecretKey, SecretMap, SecretPath, SecretsError};
use crate::config::VarSource;

/// Total request budget when the backend does not override it.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Connect budget, capped by the total so a tight `timeout_ms` stays honest.
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 2_000;

/// Matches reqwest's own default. Named here only because following redirects
/// at all is a deliberate choice once a token rides along.
const MAX_REDIRECTS: usize = 10;

/// Where the token comes from. Exactly one is declared per backend.
#[derive(Clone, Debug)]
pub enum TokenSource {
    /// A file written by `bao login`, tilde-expanded at construction.
    File(PathBuf),
    /// A literal or `{ env = "..." }` declaration.
    Var(VarSource),
}

/// Everything the backend needs, already validated.
///
/// Constructed by [`crate::secrets::config`] from `[secrets.backends.<name>]`
/// so that failures name a config key rather than surfacing at first use.
pub struct OpenBaoSettings {
    pub addr: String,
    pub mount: String,
    pub path_prefix: String,
    pub machine_id: Option<String>,
    pub token: TokenSource,
    pub ca_cert_file: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenBaoBuildError {
    #[error("`addr` must start with `http://` or `https://`, got `{0}`")]
    Scheme(String),

    #[error(
        "`addr` must use `https://` for a remote OpenBao, got `{0}`: the token travels in an \
         `X-Vault-Token` header, so plain `http://` would put it on the wire in cleartext. \
         Plain `http://` is accepted only for a loopback address."
    )]
    Insecure(String),

    #[error("could not read `ca_cert_file` at `{path}`: {cause}")]
    CaCertRead { path: PathBuf, cause: String },

    #[error("`ca_cert_file` at `{path}` is not a PEM certificate: {cause}")]
    CaCertParse { path: PathBuf, cause: String },

    #[error("could not build an HTTP client for the openbao backend: {0}")]
    Client(String),
}

#[derive(Clone)]
pub struct OpenBaoBackend {
    client: reqwest::Client,
    addr: String,
    mount: String,
    path_prefix: String,
    machine_id: Option<String>,
    token: TokenSource,
}

impl OpenBaoBackend {
    pub fn new(settings: OpenBaoSettings) -> Result<Self, OpenBaoBuildError> {
        let addr = settings.addr.trim_end_matches('/').to_string();
        if !(addr.starts_with("http://") || addr.starts_with("https://")) {
            return Err(OpenBaoBuildError::Scheme(settings.addr));
        }
        let parsed = reqwest::Url::parse(&addr).map_err(|_| OpenBaoBuildError::Scheme(settings.addr.clone()))?;
        if !carries_a_token_safely(&parsed) {
            return Err(OpenBaoBuildError::Insecure(settings.addr));
        }

        // Both budgets are explicit: an unreachable backend inside a headless
        // `trg mcp proxy` child surfaces only as MCP error -32000, so a hang is
        // the failure mode this design is most exposed to.
        let connect = settings.timeout.min(Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS));
        // reqwest strips only the headers it knows are sensitive, and
        // `X-Vault-Token` is not one of them, so a followed redirect re-sends
        // the token verbatim. The configured instance is therefore the only
        // origin allowed to receive it, and everything else is refused: a
        // remote answering 307 with somewhere else is asking for the token,
        // whether or not the target is itself https.
        let origin = parsed.origin();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(connect)
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.url().origin() != origin {
                    attempt.stop()
                } else if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else {
                    attempt.follow()
                }
            }));

        if let Some(ref pem_path) = settings.ca_cert_file {
            let pem = std::fs::read(pem_path).map_err(|e| OpenBaoBuildError::CaCertRead {
                path: pem_path.clone(),
                cause: e.to_string(),
            })?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| OpenBaoBuildError::CaCertParse {
                path: pem_path.clone(),
                cause: e.to_string(),
            })?;
            // A pinned deployment stays pinned: `tls_certs_only` replaces the
            // OS roots for this backend rather than merging with them.
            builder = builder.tls_certs_only(certs);
        }

        let client = builder.build().map_err(|e| OpenBaoBuildError::Client(e.to_string()))?;

        Ok(Self {
            client,
            addr,
            mount: settings.mount,
            path_prefix: settings.path_prefix,
            machine_id: settings.machine_id,
            token: settings.token,
        })
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn mount(&self) -> &str {
        &self.mount
    }

    pub fn machine_id(&self) -> Option<&str> {
        self.machine_id.as_deref()
    }

    /// Where the OAuth credentials for one server live.
    ///
    /// Shared across machines unless `machine_id` says otherwise, because
    /// reaching one credential from everywhere is the reason to leave the
    /// Keychain at all. Isolation is the opt-in: a provider that rotates
    /// refresh tokens and detects replay will revoke the whole grant when two
    /// machines refresh the same one, and `machine_id` is how you avoid that.
    pub fn credential_path(&self, server: &str) -> String {
        match &self.machine_id {
            Some(id) => format!("mcp/{id}/{server}"),
            None => format!("mcp/{server}"),
        }
    }

    /// The prefix `credential_path` writes under, for error messages.
    fn credential_root(&self) -> String {
        match &self.machine_id {
            Some(id) => format!("{}/{}/mcp/{id}/", self.mount, self.path_prefix),
            None => format!("{}/{}/mcp/", self.mount, self.path_prefix),
        }
    }

    /// Whether a server name can address this backend at all.
    ///
    /// Checked when the path is built rather than when it is first used, so a
    /// name that cannot work fails while the user is looking at their config.
    pub fn check_server_name(&self, server: &str) -> Result<(), String> {
        if server.is_empty() || !server.bytes().all(is_path_byte) {
            return Err(format!(
                "a server stored in OpenBao must be named with [A-Za-z0-9._-], because the name \
                 becomes a path segment at `{}`",
                self.credential_root()
            ));
        }
        Ok(())
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        let url = self.data_url(path)?;
        let response = self.send(reqwest::Method::GET, &url, path, None).await?;

        let Some(body) = self.read_body(response, path).await? else {
            return Ok(None);
        };

        let read: Envelope<ReadPayload> = serde_json::from_value(body).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: format!("response is not a KV v2 read: {e}"),
        })?;

        // Defensive. A soft-deleted version answers 404 with a null `data`,
        // which `read_body` already turns into a miss, so this only catches a
        // 200 shaped the same way.
        let Some(data) = read.data.data else {
            return Ok(None);
        };

        map_from_json(&data, path).map(Some)
    }

    pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
        let url = self.data_url(path)?;
        let mut data = Map::new();
        for key in map.sorted_keys() {
            let value = map.get(key).expect("key came from this map");
            data.insert(
                key.as_str().to_string(),
                Value::String(value.expose_secret().to_string()),
            );
        }
        let body = serde_json::to_value(Envelope { data }).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: e.to_string(),
        })?;

        let response = self.send(reqwest::Method::POST, &url, path, Some(body)).await?;
        self.read_body(response, path).await?;
        Ok(())
    }

    pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
        let url = self.metadata_url(path)?;
        let response = self.send(reqwest::Method::DELETE, &url, path, None).await?;
        self.read_body(response, path).await?;
        Ok(())
    }

    pub async fn list(&self, prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        let full = match prefix {
            Some(p) => self.full_path(p)?,
            None => self.checked_prefix()?,
        };
        let url = format!("{}/v1/{}/metadata/{full}", self.addr, self.mount);
        let anchor = prefix.cloned().unwrap_or_else(|| {
            // `list` with no prefix has no path to blame in an error, so borrow
            // the backend's own prefix for the message.
            SecretPath::parse(if self.path_prefix.is_empty() {
                "."
            } else {
                &self.path_prefix
            })
            .unwrap_or_else(|_| SecretPath::parse("openbao").expect("static path is valid"))
        });

        let method = reqwest::Method::from_bytes(b"LIST").expect("LIST is a valid method token");
        let response = self.send(method, &url, &anchor, None).await?;

        let Some(body) = self.read_body(response, &anchor).await? else {
            return Ok(Vec::new());
        };

        let listing: Envelope<ListPayload> = serde_json::from_value(body).map_err(|e| SecretsError::Malformed {
            path: anchor.clone(),
            cause: format!("response is not a KV v2 listing: {e}"),
        })?;

        let mut out = listing.data.keys;
        out.sort();
        Ok(out)
    }

    fn data_url(&self, path: &SecretPath) -> Result<String, SecretsError> {
        Ok(format!(
            "{}/v1/{}/data/{}",
            self.addr,
            self.mount,
            self.full_path(path)?
        ))
    }

    fn metadata_url(&self, path: &SecretPath) -> Result<String, SecretsError> {
        Ok(format!(
            "{}/v1/{}/metadata/{}",
            self.addr,
            self.mount,
            self.full_path(path)?
        ))
    }

    /// The bare prefix, which reaches the URL without passing through
    /// [`Self::full_path`] when `list` is given nothing to scope to.
    fn checked_prefix(&self) -> Result<String, SecretsError> {
        if self
            .path_prefix
            .split('/')
            .filter(|s| !s.is_empty())
            .all(is_addressable_segment)
        {
            return Ok(self.path_prefix.clone());
        }
        Err(SecretsError::Unavailable(format!(
            "`path_prefix` `{}` is not addressable in OpenBao: each segment must match \
             [A-Za-z0-9._-] and be neither `.` nor `..`",
            self.path_prefix
        )))
    }

    /// The prefix-joined path, checked against what is safe to put in a URL.
    ///
    /// [`SecretPath`] accepts anything a Keychain account attribute accepts,
    /// including spaces and non-ASCII, so the narrower rule is applied here at
    /// the backend that needs it rather than being imposed on every backend.
    fn full_path(&self, path: &SecretPath) -> Result<String, SecretsError> {
        let full = path.with_prefix(&self.path_prefix);
        for segment in full.split('/') {
            if !is_addressable_segment(segment) {
                return Err(SecretsError::Unauthorized {
                    path: path.clone(),
                    cause: format!(
                        "`{full}` is not addressable in OpenBao: each segment must match \
                         [A-Za-z0-9._-] and be neither empty, `.`, nor `..`"
                    ),
                });
            }
        }
        Ok(full)
    }

    /// Read the token afresh, so `bao login` recovers a running process.
    fn read_token(&self) -> Result<SecretString, SecretsError> {
        match &self.token {
            TokenSource::Var(source) => source
                .resolve()
                .map(|v| SecretString::from(v.trim().to_string()))
                .map_err(|e| SecretsError::Unauthorized {
                    path: SecretPath::parse("token").expect("static path is valid"),
                    cause: e.to_string(),
                }),
            TokenSource::File(path) => read_token_file(path),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        path: &SecretPath,
        body: Option<Value>,
    ) -> Result<reqwest::Response, SecretsError> {
        let token = self.read_token()?;
        let mut request = self
            .client
            .request(method, url)
            .header("X-Vault-Token", token.expose_secret());
        if let Some(body) = body {
            request = request.json(&body);
        }

        request.send().await.map_err(|e| self.transport_error(path, &e))
    }

    fn transport_error(&self, path: &SecretPath, e: &reqwest::Error) -> SecretsError {
        if e.is_timeout() {
            return SecretsError::Unavailable(format!(
                "OpenBao at {} did not answer in time while reading `{path}`",
                self.addr
            ));
        }
        if e.is_connect() {
            return SecretsError::Unavailable(format!("could not connect to OpenBao at {}", self.addr));
        }
        SecretsError::Transport(format!("OpenBao at {}: {e}", self.addr))
    }

    /// Classify the response. `Ok(None)` is the "no such secret" case, which
    /// every caller turns into its own empty answer.
    async fn read_body(&self, response: reqwest::Response, path: &SecretPath) -> Result<Option<Value>, SecretsError> {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status.is_success() {
            if body.trim().is_empty() {
                return Ok(None);
            }
            return serde_json::from_str(&body)
                .map(Some)
                .map_err(|e| SecretsError::Malformed {
                    path: path.clone(),
                    cause: e.to_string(),
                });
        }

        let errors = ErrorBody::of(&body);

        match status.as_u16() {
            // Both a missing secret and a missing mount answer 404; only the
            // body separates a miss from a misconfiguration.
            404 if errors.is_empty() => Ok(None),
            404 => Err(SecretsError::Unavailable(format!(
                "OpenBao at {} has no `{}` mount, or it is not a KV v2 mount: {}",
                self.addr,
                self.mount,
                errors.join("; ")
            ))),
            401 | 403 => Err(SecretsError::Unauthorized {
                path: path.clone(),
                cause: format!(
                    "OpenBao rejected the token ({}); run `bao login` and retry",
                    join_or(&errors, "permission denied")
                ),
            }),
            503 => Err(SecretsError::Unavailable(format!(
                "OpenBao at {} is sealed or standby: {}",
                self.addr,
                join_or(&errors, "service unavailable")
            ))),
            // Reached only when the redirect policy refused to follow, since
            // a redirect within the configured origin is followed before the
            // response gets here.
            other if (300..400).contains(&other) => Err(SecretsError::Transport(format!(
                "OpenBao at {} redirected {other} to another host, and the token is not sent \
                 anywhere but `addr`. Point `addr` at the active node or at a load balancer \
                 in front of the cluster.",
                self.addr
            ))),
            other => Err(SecretsError::Transport(format!(
                "OpenBao at {} answered {other}: {}",
                self.addr,
                join_or(&errors, "no error detail")
            ))),
        }
    }
}

fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// Whether one path segment addresses what it spells.
///
/// `.` and `..` are spelled with bytes [`is_path_byte`] allows, but the URL
/// parser resolves them before the request goes out, so a segment naming a
/// server would silently address a different secret.
pub(crate) fn is_addressable_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && segment.bytes().all(is_path_byte)
}

/// Whether `X-Vault-Token` may be sent to this URL in the clear.
fn carries_a_token_safely(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => match url.host_str() {
            Some("localhost") => true,
            Some(host) => host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()),
            None => false,
        },
        _ => false,
    }
}

fn join_or(errors: &[String], fallback: &str) -> String {
    if errors.is_empty() {
        fallback.to_string()
    } else {
        errors.join("; ")
    }
}

/// The values stay [`Value`] rather than deserializing straight into strings,
/// so a non-string is reported against the key that carries it instead of as a
/// serde error against the whole response.
fn map_from_json(object: &Map<String, Value>, path: &SecretPath) -> Result<SecretMap, SecretsError> {
    let mut map = SecretMap::new();
    for (name, value) in object {
        let text = value.as_str().ok_or_else(|| SecretsError::Malformed {
            path: path.clone(),
            cause: format!("key `{name}` is not a string; `trg` stores only string values"),
        })?;
        let key = SecretKey::parse(name).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: e.to_string(),
        })?;
        map.insert(key, SecretString::from(text.to_string()));
    }
    Ok(map)
}

/// Read a token file, refusing one any other user can read.
///
/// `bao login` writes `~/.vault-token` with mode 600. A file that has been
/// loosened is a live credential leak, and silently using it would hide that.
fn read_token_file(path: &Path) -> Result<SecretString, SecretsError> {
    use std::os::unix::fs::PermissionsExt as _;

    let meta = std::fs::metadata(path).map_err(|e| SecretsError::Unauthorized {
        path: SecretPath::parse("token").expect("static path is valid"),
        cause: format!(
            "could not read the token file at `{}`: {e}; run `bao login` first",
            path.display()
        ),
    })?;

    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(SecretsError::PermissionDenied(format!(
            "the token file at `{}` is readable by other users (mode {mode:04o}); run `chmod 600 {}`",
            path.display(),
            path.display()
        )));
    }

    let raw = std::fs::read_to_string(path).map_err(|e| SecretsError::Unauthorized {
        path: SecretPath::parse("token").expect("static path is valid"),
        cause: format!("could not read the token file at `{}`: {e}", path.display()),
    })?;

    let token = raw.trim();
    if token.is_empty() {
        return Err(SecretsError::Unauthorized {
            path: SecretPath::parse("token").expect("static path is valid"),
            cause: format!(
                "the token file at `{}` is empty; run `bao login` and retry",
                path.display()
            ),
        });
    }
    Ok(SecretString::from(token.to_string()))
}

/// Expand a leading `~` against `HOME`. Anything else is taken literally.
pub fn expand_tilde(raw: &str) -> PathBuf {
    expand_tilde_from(raw, std::env::var_os("HOME").map(PathBuf::from))
}

/// The half of [`expand_tilde`] that does not read the process environment, so
/// a test can pin a home directory without mutating it for every other thread.
fn expand_tilde_from(raw: &str, home: Option<PathBuf>) -> PathBuf {
    let Some(rest) = raw.strip_prefix('~') else {
        return PathBuf::from(raw);
    };
    let Some(home) = home else {
        return PathBuf::from(raw);
    };
    match rest.strip_prefix('/') {
        Some(tail) => home.join(tail),
        None if rest.is_empty() => home,
        // `~other/...` names another user's home, which is not expanded.
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::kv_v2::{VersionMetadata, WritePayload};
    use super::*;

    /// One request the backend actually put on the wire.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        url: String,
        token: Option<String>,
        body: String,
    }

    /// A synthetic timestamp. Nothing reads these fields; they exist so a
    /// fixture is the same shape as a real response.
    const WHENEVER: &str = "1970-01-01T00:00:00Z";

    fn version_metadata(version: u64, deleted: bool) -> VersionMetadata {
        VersionMetadata {
            version,
            created_time: WHENEVER.to_string(),
            deletion_time: if deleted { WHENEVER.to_string() } else { String::new() },
            destroyed: false,
            custom_metadata: None,
        }
    }

    /// One scripted answer, named for what OpenBao does rather than spelled
    /// out as a status and a JSON literal.
    ///
    /// The status each case carries is written down once, here, instead of at
    /// every call site, which is what stops a fixture from claiming a shape
    /// the server never sends.
    struct Reply {
        status: u16,
        body: String,
    }

    impl Reply {
        fn json<T: serde::Serialize>(status: u16, payload: &T) -> Self {
            Self {
                status,
                body: serde_json::to_string(payload).expect("a fixture serializes"),
            }
        }

        /// `GET` on a live version.
        fn hit(pairs: &[(&str, &str)]) -> Self {
            let data = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
                .collect();
            Self::json(
                200,
                &Envelope {
                    data: ReadPayload {
                        data: Some(data),
                        metadata: Some(version_metadata(1, false)),
                    },
                },
            )
        }

        /// `GET` on a path never written. The `errors` array is present and
        /// empty, which is the only thing separating this from a bad mount.
        fn miss() -> Self {
            Self::json(404, &ErrorBody::default())
        }

        /// `GET` on a soft-deleted version: `404`, the version metadata still
        /// there, and no `errors` key at all.
        fn soft_deleted() -> Self {
            Self::json(
                404,
                &Envelope {
                    data: ReadPayload {
                        data: None,
                        metadata: Some(version_metadata(1, true)),
                    },
                },
            )
        }

        /// A `200` carrying a null `data`, which no observed OpenBao sends.
        fn ok_with_no_data() -> Self {
            Self::json(
                200,
                &Envelope {
                    data: ReadPayload {
                        data: None,
                        metadata: Some(version_metadata(4, false)),
                    },
                },
            )
        }

        /// `POST` accepted.
        fn written(version: u64) -> Self {
            Self::json(
                200,
                &Envelope {
                    data: WritePayload {
                        version,
                        created_time: WHENEVER.to_string(),
                        deletion_time: String::new(),
                        destroyed: false,
                        custom_metadata: None,
                    },
                },
            )
        }

        /// `LIST` on a folder. A key naming a folder ends in `/`.
        fn listed(keys: &[&str]) -> Self {
            Self::json(
                200,
                &Envelope {
                    data: ListPayload {
                        keys: keys.iter().map(|k| (*k).to_string()).collect(),
                    },
                },
            )
        }

        /// `DELETE` accepted, with nothing to say.
        fn no_content() -> Self {
            Self {
                status: 204,
                body: String::new(),
            }
        }

        /// Any refusal that carries OpenBao's `errors` array.
        fn refused(status: u16, errors: &[&str]) -> Self {
            Self::json(
                status,
                &ErrorBody {
                    errors: errors.iter().map(|e| (*e).to_string()).collect(),
                },
            )
        }

        /// Something that is not OpenBao answering, such as an intercepting
        /// proxy, or a status the client does not classify.
        fn verbatim(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_string(),
            }
        }
    }

    /// A real HTTP server answering scripted responses on a loopback port.
    ///
    /// The backend under test is the production one, talking to it through the
    /// production `reqwest` client, so the status-code table these tests pin
    /// down is exercised through the same code path a live OpenBao would take.
    struct StubBao {
        addr: String,
        seen: Arc<Mutex<Vec<Seen>>>,
        server: Arc<tiny_http::Server>,
    }

    impl StubBao {
        fn start(replies: Vec<Reply>) -> Self {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
            let addr = format!(
                "http://{}",
                server.server_addr().to_ip().expect("a loopback tcp address")
            );
            let seen = Arc::new(Mutex::new(Vec::new()));

            let worker = Arc::clone(&server);
            let recorder = Arc::clone(&seen);
            std::thread::spawn(move || {
                for reply in replies {
                    let Ok(mut request) = worker.recv() else { return };

                    let mut raw = String::new();
                    let _ = request.as_reader().read_to_string(&mut raw);
                    recorder.lock().expect("stub lock").push(Seen {
                        method: request.method().as_str().to_string(),
                        url: request.url().to_string(),
                        token: request
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("X-Vault-Token"))
                            .map(|h| h.value.as_str().to_string()),
                        body: raw,
                    });

                    let response = if reply.body.is_empty() {
                        tiny_http::Response::empty(reply.status).boxed()
                    } else {
                        tiny_http::Response::from_string(reply.body)
                            .with_status_code(reply.status)
                            .with_header(
                                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                    .expect("static header"),
                            )
                            .boxed()
                    };
                    let _ = request.respond(response);
                }
            });

            Self { addr, seen, server }
        }

        /// Answers the first request with a 307 to `location` and every later
        /// one with a hit, recording each so a test can see whether the token
        /// travelled and where.
        fn start_redirecting_once_to(location: String) -> Self {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
            let addr = format!(
                "http://{}",
                server.server_addr().to_ip().expect("a loopback tcp address")
            );
            let seen = Arc::new(Mutex::new(Vec::new()));

            let worker = Arc::clone(&server);
            let recorder = Arc::clone(&seen);
            std::thread::spawn(move || {
                let mut first = true;
                while let Ok(request) = worker.recv() {
                    recorder.lock().expect("stub lock").push(Seen {
                        method: request.method().as_str().to_string(),
                        url: request.url().to_string(),
                        token: request
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("X-Vault-Token"))
                            .map(|h| h.value.as_str().to_string()),
                        body: String::new(),
                    });

                    let response = if std::mem::take(&mut first) {
                        tiny_http::Response::empty(307)
                            .with_header(
                                tiny_http::Header::from_bytes(&b"Location"[..], location.as_bytes())
                                    .expect("a location header"),
                            )
                            .boxed()
                    } else {
                        let hit = Reply::hit(&[("k", "v")]);
                        tiny_http::Response::from_string(hit.body)
                            .with_status_code(hit.status)
                            .boxed()
                    };
                    let _ = request.respond(response);
                }
            });

            Self { addr, seen, server }
        }

        fn backend(&self) -> OpenBaoBackend {
            let mut s = settings(&self.addr);
            s.timeout = Duration::from_secs(2);
            OpenBaoBackend::new(s).expect("build")
        }

        fn requests(&self) -> Vec<Seen> {
            self.seen.lock().expect("stub lock").clone()
        }

        fn only_request(&self) -> Seen {
            let seen = self.requests();
            assert_eq!(seen.len(), 1, "expected exactly one request, got {seen:?}");
            seen.into_iter().next().expect("checked above")
        }
    }

    impl Drop for StubBao {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    fn path(raw: &str) -> SecretPath {
        SecretPath::parse(raw).expect("parse")
    }

    fn settings(addr: &str) -> OpenBaoSettings {
        OpenBaoSettings {
            addr: addr.to_string(),
            mount: "secret".to_string(),
            path_prefix: "trg".to_string(),
            machine_id: Some("laptop".to_string()),
            token: TokenSource::Var(VarSource::Literal("t".to_string())),
            ca_cert_file: None,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }

    fn backend(addr: &str) -> OpenBaoBackend {
        OpenBaoBackend::new(settings(addr)).expect("build")
    }

    #[test]
    fn plain_http_is_refused_for_a_remote_backend() {
        for addr in [
            "http://bao.example.com:8200",
            "http://10.0.0.5:8200",
            "http://[2001:db8::1]:8200",
        ] {
            assert!(
                matches!(OpenBaoBackend::new(settings(addr)), Err(OpenBaoBuildError::Insecure(_))),
                "the token would go out in cleartext to {addr}"
            );
        }
    }

    #[test]
    fn plain_http_is_allowed_for_a_loopback_backend() {
        for addr in ["http://127.0.0.1:8200", "http://localhost:8200", "http://[::1]:8200"] {
            assert!(
                OpenBaoBackend::new(settings(addr)).is_ok(),
                "{addr} never leaves the host"
            );
        }
    }

    /// reqwest strips `Authorization` across origins but not `X-Vault-Token`,
    /// so nothing but the policy keeps the token off a third party. Both stubs
    /// are loopback http, which the scheme check accepts: only the origin
    /// check can refuse this one.
    #[tokio::test]
    async fn a_redirect_to_another_origin_never_carries_the_token() {
        let elsewhere = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
        let bao = StubBao::start_redirecting_once_to(format!("{}/v1/secret/data/trg/mcp/laptop/x", elsewhere.addr));

        let err = bao
            .backend()
            .get(&path("mcp/laptop/x"))
            .await
            .expect_err("the redirect is refused");

        assert!(
            elsewhere.requests().is_empty(),
            "the token reached a third party: {:?}",
            elsewhere.requests()
        );
        let SecretsError::Transport(reason) = err else {
            panic!("expected a transport error, got {err:?}")
        };
        assert!(reason.contains("307"), "{reason}");
        assert!(reason.contains("not sent"), "{reason}");
    }

    /// The instance moving a request around inside itself is ordinary, and
    /// the token is already its own.
    #[tokio::test]
    async fn a_redirect_within_the_configured_origin_is_followed() {
        let bao = StubBao::start_redirecting_once_to("/v1/secret/data/trg/elsewhere".to_string());

        let hit = bao.backend().get(&path("mcp/laptop/x")).await.expect("followed");

        assert!(hit.is_some(), "the redirect target answered a hit");
        let seen = bao.requests();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[1].url, "/v1/secret/data/trg/elsewhere");
        assert_eq!(seen[1].token.as_deref(), Some("t"));
    }

    #[test]
    fn a_redirect_may_not_downgrade_the_token_onto_cleartext() {
        let https = reqwest::Url::parse("https://bao.example.com/v1/x").expect("url");
        let remote_http = reqwest::Url::parse("http://bao.example.com/v1/x").expect("url");
        let loopback_http = reqwest::Url::parse("http://127.0.0.1:8200/v1/x").expect("url");

        assert!(carries_a_token_safely(&https));
        assert!(carries_a_token_safely(&loopback_http));
        assert!(!carries_a_token_safely(&remote_http));
    }

    /// `SecretPath` already refuses a dot segment, so the prefix is the only
    /// way one can reach the joined path that becomes a URL.
    #[test]
    fn a_dot_segment_in_the_prefix_cannot_walk_out_of_it() {
        for prefix in ["trg/..", "..", "trg/."] {
            let mut s = settings("https://bao.example.com:8200");
            s.path_prefix = prefix.to_string();
            let b = OpenBaoBackend::new(s).expect("build");

            assert!(
                matches!(b.full_path(&path("github")), Err(SecretsError::Unauthorized { .. })),
                "should refuse the prefix {prefix:?}"
            );
        }
    }

    #[test]
    fn a_prefix_with_a_dot_segment_is_refused_when_list_has_nothing_to_scope_to() {
        let mut s = settings("https://bao.example.com:8200");
        s.path_prefix = "trg/..".to_string();
        let b = OpenBaoBackend::new(s).expect("build");

        assert!(matches!(b.checked_prefix(), Err(SecretsError::Unavailable(_))));
    }

    #[test]
    fn tilde_expands_against_the_home_it_is_given() {
        let home = Some(PathBuf::from("/home/tester"));
        assert_eq!(
            expand_tilde_from("~/.vault-token", home.clone()),
            PathBuf::from("/home/tester/.vault-token")
        );
        assert_eq!(expand_tilde_from("~", home.clone()), PathBuf::from("/home/tester"));
        assert_eq!(
            expand_tilde_from("/etc/token", home.clone()),
            PathBuf::from("/etc/token")
        );
        assert_eq!(expand_tilde_from("~other/token", home), PathBuf::from("~other/token"));
    }

    #[test]
    fn a_tilde_survives_unexpanded_when_there_is_no_home() {
        assert_eq!(
            expand_tilde_from("~/.vault-token", None),
            PathBuf::from("~/.vault-token")
        );
    }

    #[test]
    fn rejects_an_addr_without_a_scheme() {
        let mut s = settings("bao.example.com:8200");
        s.addr = "bao.example.com:8200".to_string();
        assert!(matches!(OpenBaoBackend::new(s), Err(OpenBaoBuildError::Scheme(_))));
    }

    #[test]
    fn trailing_slashes_do_not_double_up_in_urls() {
        let b = backend("https://bao.example.com:8200/");
        let path = SecretPath::parse("mcp/laptop/github").expect("parse");
        assert_eq!(
            b.data_url(&path).expect("url"),
            "https://bao.example.com:8200/v1/secret/data/trg/mcp/laptop/github"
        );
        assert_eq!(
            b.metadata_url(&path).expect("url"),
            "https://bao.example.com:8200/v1/secret/metadata/trg/mcp/laptop/github"
        );
    }

    #[test]
    fn an_empty_path_prefix_does_not_leave_a_double_slash() {
        let mut s = settings("https://bao.example.com:8200");
        s.path_prefix = String::new();
        let b = OpenBaoBackend::new(s).expect("build");
        let path = SecretPath::parse("mcp/laptop/github").expect("parse");
        assert_eq!(
            b.data_url(&path).expect("url"),
            "https://bao.example.com:8200/v1/secret/data/mcp/laptop/github"
        );
    }

    #[test]
    fn a_machine_id_scopes_the_credential_path_to_that_machine() {
        let b = backend("https://bao.example.com:8200");
        assert_eq!(b.credential_path("github"), "mcp/laptop/github");
    }

    /// The reason to leave the Keychain is that one credential is reachable
    /// from everywhere, so sharing is what you get when you ask for nothing.
    #[test]
    fn without_a_machine_id_every_machine_shares_one_credential() {
        let mut s = settings("https://bao.example.com:8200");
        s.machine_id = None;
        let b = OpenBaoBackend::new(s).expect("build");

        assert_eq!(b.credential_path("github"), "mcp/github");
        assert_eq!(b.machine_id(), None);
    }

    #[test]
    fn an_unaddressable_server_name_is_blamed_on_the_path_it_would_take() {
        let mut shared = settings("https://bao.example.com:8200");
        shared.machine_id = None;
        let shared = OpenBaoBackend::new(shared).expect("build");
        let reason = shared.check_server_name("my server").expect_err("refused");
        assert!(reason.contains("secret/trg/mcp/"), "{reason}");
        assert!(!reason.contains("laptop"), "{reason}");

        let scoped = backend("https://bao.example.com:8200");
        let reason = scoped.check_server_name("my server").expect_err("refused");
        assert!(reason.contains("secret/trg/mcp/laptop/"), "{reason}");
    }

    #[test]
    fn paths_a_keychain_accepts_but_a_url_should_not_are_refused() {
        let b = backend("https://bao.example.com:8200");
        for raw in ["my server", "Ünïcode", "a b/c", "we?rd"] {
            let path = SecretPath::parse(raw).expect("structurally valid");
            assert!(
                matches!(b.full_path(&path), Err(SecretsError::Unauthorized { .. })),
                "should refuse {raw:?}"
            );
        }
    }

    #[test]
    fn ordinary_names_are_addressable() {
        let b = backend("https://bao.example.com:8200");
        for raw in ["github", "my-server", "a.b", "mcp/laptop/x_1"] {
            let path = SecretPath::parse(raw).expect("parse");
            assert!(b.full_path(&path).is_ok(), "should accept {raw:?}");
        }
    }

    #[test]
    fn errors_of_extracts_only_the_errors_array() {
        assert_eq!(ErrorBody::of(r#"{"errors":[]}"#), Vec::<String>::new());
        assert_eq!(
            ErrorBody::of(r#"{"errors":["permission denied"]}"#),
            ["permission denied"]
        );
        assert_eq!(ErrorBody::of("not json"), Vec::<String>::new());
        assert_eq!(ErrorBody::of(r#"{"data":{"secret":"hunter2"}}"#), Vec::<String>::new());
    }

    #[test]
    fn map_from_json_rejects_non_string_values() {
        let path = SecretPath::parse("x").expect("parse");
        let data: Map<String, Value> = serde_json::from_str(r#"{"a":1}"#).expect("json");
        assert!(matches!(
            map_from_json(&data, &path),
            Err(SecretsError::Malformed { .. })
        ));
    }

    #[test]
    fn map_from_json_reads_string_values() {
        let path = SecretPath::parse("x").expect("parse");
        let data: Map<String, Value> = serde_json::from_str(r#"{"a":"1","b":"2"}"#).expect("json");
        let map = map_from_json(&data, &path).expect("map");
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&SecretKey::parse("a").unwrap()).map(|v| v.expose_secret()),
            Some("1")
        );
    }

    #[test]
    fn a_group_readable_token_file_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "s.abc").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");

        assert!(matches!(read_token_file(&path), Err(SecretsError::PermissionDenied(_))));
    }

    #[test]
    fn a_private_token_file_reads_and_trims() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "s.abc\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        assert_eq!(read_token_file(&path).expect("read").expose_secret(), "s.abc");
    }

    #[test]
    fn an_empty_token_file_names_the_recovery_command() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "  \n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let err = read_token_file(&path).expect_err("should refuse");
        assert!(err.to_string().contains("bao login"), "{err}");
    }

    #[tokio::test]
    async fn a_hit_reads_the_nested_data_object() {
        let stub = StubBao::start(vec![Reply::hit(&[("credentials", r#"{"a":1}"#), ("other", "x")])]);
        let map = stub
            .backend()
            .get(&path("mcp/laptop/github"))
            .await
            .expect("get")
            .expect("some");

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&SecretKey::parse("credentials").unwrap())
                .map(|v| v.expose_secret()),
            Some(r#"{"a":1}"#)
        );

        let seen = stub.only_request();
        assert_eq!(seen.method, "GET");
        assert_eq!(seen.url, "/v1/secret/data/trg/mcp/laptop/github");
        assert_eq!(seen.token.as_deref(), Some("t"));
    }

    #[tokio::test]
    async fn a_missing_secret_is_a_miss_not_an_error() {
        let stub = StubBao::start(vec![Reply::miss()]);
        assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn a_missing_mount_is_a_configuration_error_not_a_miss() {
        let stub = StubBao::start(vec![Reply::refused(
            404,
            &[r#"no handler for route "nope/data/x". route entry not found."#],
        )]);
        let err = stub.backend().get(&path("x")).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
        assert!(err.to_string().contains("secret"), "{err}");
    }

    #[tokio::test]
    async fn a_soft_deleted_version_reads_as_a_miss() {
        // A live OpenBao 2.5.5 answers `DELETE /v1/<mount>/data/<path>` and then
        // a read of that path with a `404` whose body carries `data` but no
        // `errors`, which is what [`Reply::soft_deleted`] reproduces.
        let stub = StubBao::start(vec![Reply::soft_deleted()]);
        assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
    }

    /// The guard in `get`, which no observed OpenBao response reaches.
    #[tokio::test]
    async fn a_success_carrying_a_null_data_object_is_also_a_miss() {
        let stub = StubBao::start(vec![Reply::ok_with_no_data()]);
        assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn a_rejected_token_names_the_recovery_command() {
        let stub = StubBao::start(vec![Reply::refused(403, &["permission denied"])]);
        let err = stub.backend().get(&path("x")).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Unauthorized { .. }), "{err:?}");
        assert!(err.to_string().contains("bao login"), "{err}");
    }

    #[tokio::test]
    async fn a_sealed_backend_is_unavailable() {
        let stub = StubBao::start(vec![Reply::refused(503, &["Vault is sealed"])]);
        let err = stub.backend().get(&path("x")).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
        assert!(err.to_string().contains("sealed"), "{err}");
    }

    #[tokio::test]
    async fn an_unclassified_status_never_repeats_the_response_body() {
        let stub = StubBao::start(vec![Reply::verbatim(500, r#"{"data":{"credentials":"hunter2"}}"#)]);
        let err = stub.backend().get(&path("x")).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
        assert!(!err.to_string().contains("hunter2"), "{err}");
    }

    #[tokio::test]
    async fn a_success_that_is_not_json_is_malformed() {
        let stub = StubBao::start(vec![Reply::verbatim(200, "<html>proxy says no</html>")]);
        let err = stub.backend().get(&path("x")).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn set_wraps_the_map_in_the_kv_v2_data_envelope() {
        let stub = StubBao::start(vec![Reply::written(1)]);
        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse("credentials").unwrap(),
            SecretString::from("v".to_string()),
        );

        stub.backend().set(&path("mcp/laptop/github"), &map).await.expect("set");

        let seen = stub.only_request();
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.url, "/v1/secret/data/trg/mcp/laptop/github");
        assert_eq!(seen.body, r#"{"data":{"credentials":"v"}}"#);
    }

    #[tokio::test]
    async fn delete_addresses_metadata_and_accepts_an_empty_204() {
        let stub = StubBao::start(vec![Reply::no_content()]);
        stub.backend().delete(&path("mcp/laptop/github")).await.expect("delete");

        let seen = stub.only_request();
        assert_eq!(seen.method, "DELETE");
        assert_eq!(seen.url, "/v1/secret/metadata/trg/mcp/laptop/github");
    }

    /// A per-user subtree is spelled as a multi-segment `path_prefix`, which
    /// has to reach the wire with its segments intact for a templated ACL
    /// path to match it.
    #[tokio::test]
    async fn a_multi_segment_path_prefix_keeps_its_segments() {
        let stub = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
        let mut s = settings(&stub.addr);
        s.path_prefix = "trg/alice".to_string();
        s.machine_id = None;
        s.timeout = Duration::from_secs(2);
        let backend = OpenBaoBackend::new(s).expect("build");

        backend.get(&path("mcp/internal")).await.expect("get").expect("some");

        assert_eq!(stub.only_request().url, "/v1/secret/data/trg/alice/mcp/internal");
    }

    #[tokio::test]
    async fn list_sorts_the_keys_it_is_given() {
        let stub = StubBao::start(vec![Reply::listed(&["zeta", "alpha", "mid/"])]);
        let keys = stub.backend().list(None).await.expect("list");

        assert_eq!(keys, ["alpha", "mid/", "zeta"]);

        let seen = stub.only_request();
        assert_eq!(seen.method, "LIST");
        assert_eq!(seen.url, "/v1/secret/metadata/trg");
    }

    #[tokio::test]
    async fn list_of_an_empty_folder_is_empty_not_an_error() {
        let stub = StubBao::start(vec![Reply::miss()]);
        assert!(stub.backend().list(None).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn the_token_is_re_read_for_every_operation() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let token = dir.path().join("token");
        std::fs::write(&token, "first").expect("write");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let stub = StubBao::start(vec![Reply::miss(), Reply::miss()]);
        let mut s = settings(&stub.addr);
        s.token = TokenSource::File(token.clone());
        let backend = OpenBaoBackend::new(s).expect("build");

        backend.get(&path("x")).await.expect("first get");
        std::fs::write(&token, "second").expect("rewrite");
        backend.get(&path("x")).await.expect("second get");

        let tokens: Vec<Option<String>> = stub.requests().into_iter().map(|r| r.token).collect();
        assert_eq!(
            tokens,
            [Some("first".to_string()), Some("second".to_string())],
            "a token refreshed by `bao login` must be picked up without a restart"
        );
    }

    #[tokio::test]
    async fn an_unreachable_backend_fails_instead_of_hanging() {
        // 203.0.113.0/24 is TEST-NET-3, reserved for documentation and
        // guaranteed not to be routed, so this connect can only time out.
        let mut s = settings("https://203.0.113.1:8200");
        s.timeout = Duration::from_millis(300);
        let b = OpenBaoBackend::new(s).expect("build");
        let path = SecretPath::parse("x").expect("parse");

        let started = std::time::Instant::now();
        let err = b.get(&path).await.expect_err("should not succeed");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(err, SecretsError::Unavailable(_)),
            "should be unavailable, got {err:?}"
        );
    }
}
