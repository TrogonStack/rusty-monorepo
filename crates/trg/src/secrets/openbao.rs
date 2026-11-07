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

use std::os::unix::fs::PermissionsExt as _;
mod constants;
pub use constants::DEFAULT_TIMEOUT_MS;
use constants::{DATA_ROUTE, DEFAULT_CONNECT_TIMEOUT_MS, HEALTH_ROUTE, MAX_REDIRECTS, METADATA_ROUTE};
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use tracing::Instrument;

use super::kv_v2::{Envelope, ErrorBody, ListPayload, ReadPayload};
use super::telemetry::{HttpSpan, ServerAddress, UrlTemplate};
use super::{BackendKind, SecretKey, SecretMap, SecretPath, SecretsError};
use crate::config::{FetchedSecrets, RawVarSource, VarResolveError};

/// How a config var addresses one value in this backend.
///
/// The path is relative to the backend's mount, prefix and owner, which is
/// why it is not the same type as [`super::KeychainReference`] despite being
/// spelled the same way: the two are joined onto entirely different things.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpenbaoReference {
    path: SecretPath,
    key: SecretKey,
}

impl OpenbaoReference {
    pub fn new(path: SecretPath, key: SecretKey) -> Self {
        Self { path, key }
    }

    pub fn path(&self) -> &SecretPath {
        &self.path
    }

    pub fn key(&self) -> &SecretKey {
        &self.key
    }
}

/// Where the token comes from. Exactly one is declared per backend.
#[derive(Clone, Debug)]
pub enum TokenSource {
    /// A file written by `bao login`, tilde-expanded at construction.
    File(PathBuf),
    /// A literal or `{ env = "..." }` declaration.
    Var(RawVarSource),
}

/// Why no token could be presented.
///
/// Every one of these is a failure to *obtain* a credential, as opposed to one
/// the instance looked at and refused, so they carry no secret path: nothing
/// was addressed and nothing was denied.
///
/// The recovery advice lives in [`TokenError::remedy`] rather than in these
/// messages, so the report that has a place for it can put it there and the
/// one that does not can append it.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("could not read the token file at `{}`", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A token file another user can read is a live credential leak, and
    /// silently using it would hide that.
    #[error("the token file at `{}` is readable by other users (mode {mode:04o})", path.display())]
    LooseMode { path: PathBuf, mode: u32 },

    #[error("the token file at `{}` is empty", path.display())]
    Empty { path: PathBuf },

    #[error(transparent)]
    Var(#[from] VarResolveError),
}

impl TokenError {
    /// This error and the chain under it, on one line.
    ///
    /// These name what failed and leave why to a `#[source]`, so a caller that
    /// printed only the outer message would say a token file could not be read
    /// without saying whether it was absent, unreadable, or something else.
    pub fn report(&self) -> String {
        let mut out = self.to_string();
        let mut cause = std::error::Error::source(self);
        while let Some(e) = cause {
            out.push_str(&format!(": {e}"));
            cause = e.source();
        }
        out
    }

    /// What to do about it, phrased as a command where there is one.
    pub fn remedy(&self) -> String {
        match self {
            TokenError::Unreadable { .. } | TokenError::Empty { .. } => "run `bao login` to write one".to_string(),
            TokenError::LooseMode { path, .. } => format!("run `chmod 600 {}`", path.display()),
            TokenError::Var(_) => "set the variable the config names".to_string(),
        }
    }
}

/// The part of `sys/health` this crate reads.
///
/// The three flags are required, unlike the version metadata elsewhere, because
/// they are what makes a body a health report at all. Defaulting them lets any
/// JSON object parse as one, so an `addr` pointed at something that is not
/// OpenBao reads back as an OpenBao that was never initialized, and the report
/// then tells the operator to initialize whatever is actually listening there.
/// `version` only decorates the summary, so a deployment that omits it is still
/// worth reporting on.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Health {
    pub initialized: bool,
    pub sealed: bool,
    pub standby: bool,
    #[serde(default)]
    pub version: String,
}

impl Health {
    /// One line an operator can read, in the order that matters: an
    /// uninitialized or sealed instance cannot serve anything, and a standby
    /// serves only after forwarding.
    pub fn summary(&self) -> String {
        let state = if !self.initialized {
            "uninitialized"
        } else if self.sealed {
            "sealed"
        } else if self.standby {
            "unsealed, standby"
        } else {
            "unsealed, active"
        };

        if self.version.is_empty() {
            state.to_string()
        } else {
            format!("{state} (OpenBao {})", self.version)
        }
    }

    /// Whether the instance can serve a request right now.
    pub fn is_serving(&self) -> bool {
        self.initialized && !self.sealed
    }
}

/// What a `404` carrying no `errors` array means to the caller that asked.
///
/// A read that finds nothing found nothing. A write that finds nothing wrote
/// nothing, which is what something standing in front of OpenBao answers for a
/// path it does not know, so reporting it as a stored credential would lose the
/// credential silently.
#[derive(Clone, Copy)]
enum Absence {
    IsAMiss,
    IsAFailure,
}

/// Everything the backend needs.
///
/// Whatever ends up in a URL is checked by [`OpenBaoBackend::new`], so a value
/// that cannot address anything is refused while the config that spelled it is
/// still in view rather than at the first secret operation.
pub struct OpenBaoSettings {
    pub addr: String,
    pub mount: String,
    pub path_prefix: String,
    /// Whose credentials these are. A person is one owner across every
    /// machine they use, so this is stable in a config shared between them.
    pub owner: String,
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
    CaCertRead {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("`ca_cert_file` at `{path}` is not a PEM certificate: {cause}")]
    CaCertParse {
        path: PathBuf,
        #[source]
        cause: reqwest::Error,
    },

    #[error(
        "`ca_cert_file` at `{path}` holds no PEM certificate: pinning to it would trust nothing, \
         and every request would fail the handshake"
    )]
    CaCertEmpty { path: PathBuf },

    #[error("could not build an HTTP client for the openbao backend: {0}")]
    Client(#[source] reqwest::Error),

    #[error("`{field}` must match [A-Za-z0-9._-] and be neither empty, `.`, nor `..`, got `{value}`")]
    Segment { field: &'static str, value: String },
}

impl OpenBaoBuildError {
    /// Whether the failure comes from this machine rather than from the
    /// config, so that another machine with the same config would build.
    ///
    /// A `ca_cert_file` this host cannot open is the one such case. A file
    /// that was read but is not a usable certificate is wrong wherever it is
    /// read, and so is every malformed or unsafe value. A client that cannot be
    /// constructed is left as a stop because nothing says it is host-specific.
    pub fn is_machine_local(&self) -> bool {
        match self {
            Self::CaCertRead { .. } => true,
            Self::Scheme(_)
            | Self::Insecure(_)
            | Self::CaCertParse { .. }
            | Self::CaCertEmpty { .. }
            | Self::Client(_)
            | Self::Segment { .. } => false,
        }
    }
}

#[derive(Clone)]
pub struct OpenBaoBackend {
    client: reqwest::Client,
    addr: String,
    server: ServerAddress,
    mount: String,
    path_prefix: String,
    owner: String,
    machine_id: Option<String>,
    token: TokenSource,
}

impl OpenBaoBackend {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "Malformed backend URLs are classified as scheme validation failures under the established configuration error contract"
        )
    )]
    pub fn new(settings: OpenBaoSettings) -> Result<Self, OpenBaoBuildError> {
        let addr = settings.addr.trim_end_matches('/').to_string();
        if !(addr.starts_with("http://") || addr.starts_with("https://")) {
            return Err(OpenBaoBuildError::Scheme(settings.addr));
        }
        let parsed = reqwest::Url::parse(&addr).map_err(|_| OpenBaoBuildError::Scheme(settings.addr.clone()))?;
        if !carries_a_token_safely(&parsed) {
            return Err(OpenBaoBuildError::Insecure(settings.addr));
        }

        // These are what a URL is built out of, so they are checked here
        // rather than at the first secret operation, when the config that
        // spelled them is no longer in view.
        check_segment("mount", &settings.mount)?;
        check_segment("owner", &settings.owner)?;
        if let Some(id) = &settings.machine_id {
            check_segment("machine_id", id)?;
        }
        // Trimmed before checking, so a leading or trailing slash is a
        // spelling of the same prefix, but an interior empty segment is not.
        let path_prefix = settings.path_prefix.trim_matches('/').to_string();
        if !path_prefix.is_empty() {
            for segment in path_prefix.split('/') {
                check_segment("path_prefix", segment)?;
            }
        }

        // Both budgets are explicit: an unreachable backend inside a headless
        // `trg mcp proxy` child surfaces only as MCP error -32000, so a hang is
        // the failure mode this design is most exposed to.
        let connect = settings.timeout.min(Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS));
        let builder = reqwest::Client::builder()
            .connect_timeout(connect)
            .timeout(settings.timeout)
            .redirect(only_this_origin(&parsed));

        let client = trusting_only(builder, settings.ca_cert_file.as_deref())?
            .build()
            .map_err(OpenBaoBuildError::Client)?;

        Ok(Self {
            client,
            addr,
            server: ServerAddress::of(&parsed),
            mount: settings.mount,
            path_prefix,
            owner: settings.owner,
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

    /// Where this backend puts things, phrased for a person reading command
    /// output. It lives beside the address and mount it quotes so that
    /// renaming one cannot leave a copy of the other behind somewhere else in
    /// the tree.
    pub fn describe(&self) -> String {
        format!("OpenBao at {} (mount `{}`)", self.addr, self.mount)
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Where the token comes from, for reporting. Never the token itself.
    pub fn token_source(&self) -> &TokenSource {
        &self.token
    }

    /// Whether the configured token can be read at all, discarding it.
    ///
    /// The value never leaves this method, so a caller can report that the
    /// source works without ever holding what it produced.
    pub fn token_is_readable(&self) -> Result<(), TokenError> {
        self.read_token().map(|_| ())
    }

    /// What the instance says about itself.
    ///
    /// `sys/health` takes no token, so it answers even when the token is
    /// rejected, which is what separates an instance that is down from one
    /// that will not talk to this caller.
    ///
    /// It also reports state through the status code rather than through
    /// failure: `429` is an unsealed standby, `473` a DR secondary, `501`
    /// uninitialized, `503` sealed. Treating those as errors would report a
    /// perfectly healthy standby as unreachable, so only a transport failure
    /// is an error here.
    pub async fn health(&self) -> Result<Health, SecretsError> {
        let call = HttpSpan::start(BackendKind::Openbao, &reqwest::Method::GET, HEALTH_ROUTE, &self.server);
        let path = SecretPath::parse("sys/health").expect("static path is valid");
        let result = async {
            let response = self
                .client
                .get(format!("{}/v1/sys/health", self.addr))
                .send()
                .await
                .map_err(|e| self.transport_error(&path, &e))?;

            let status = response.status();
            call.responded(status);
            // A body that never finished arriving is a transport failure, not a
            // health report the instance got wrong.
            let body = response.text().await.map_err(|e| self.transport_error(&path, &e))?;

            serde_json::from_str(&body).map_err(|e| SecretsError::Malformed {
                path: path.clone(),
                cause: format!(
                    "`sys/health` answered {} with something other than a health report: {e}",
                    status.as_u16()
                ),
                raw: None,
            })
        }
        .instrument(call.span().clone())
        .await;

        // Health reports state through its status (`429` standby, `503`
        // sealed), so only a failure to get a report marks the span failed.
        if let Err(e) = &result {
            call.failed(e.error_type());
        }
        result
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
    ///
    /// Turning `machine_id` on or off relocates this path, so a credential
    /// written under the other layout would otherwise go quiet: the new path
    /// misses, the old payload is still sitting there, and every server asks
    /// for a fresh login. This path carries no version segment of its own;
    /// instead, the OAuth credential store reads this path as the primary
    /// and falls back to [`Self::shared_credential_path`] when the primary
    /// is empty. A version segment was deliberately avoided: it invites an
    /// open-ended ladder, one probe per layout that ever existed, and leaves
    /// a miss indistinguishable from a first-time login. Only two layouts
    /// are reachable here, so a single fallback closes the set for good.
    ///
    /// That fallback stays read-only. Copying the shared credential into the
    /// machine-scoped path would leave the shared one readable, so both
    /// machines would go on refreshing the same grant, which is the replay
    /// `machine_id` was set to prevent; moving it would break the machine
    /// that did not run the migration. The store reads it and says so
    /// instead, leaving the person to re-authorize once when it suits them,
    /// rather than at the moment they edited their config.
    pub fn credential_path(&self, server: &str) -> String {
        match &self.machine_id {
            Some(id) => format!("mcp/{id}/{server}"),
            None => format!("mcp/{server}"),
        }
    }

    /// The pre-`machine_id` shared path for this server, to fall back onto
    /// when the machine-scoped primary is empty.
    ///
    /// `None` when no `machine_id` is configured: then [`Self::credential_path`]
    /// already returns this same shared path, and there is nothing left to
    /// fall back onto.
    pub fn shared_credential_path(&self, server: &str) -> Option<String> {
        self.machine_id.is_some().then(|| format!("mcp/{server}"))
    }

    /// Everything this backend stores lives under `path_prefix`, then
    /// `owner`.
    ///
    /// Kept apart from the configured `path_prefix` so an error about an
    /// unaddressable prefix can still name the key that was written.
    /// The subtree every path is resolved under, which is what `list` scopes
    /// to and therefore what a reader needs to see to know where they are.
    pub fn storage_prefix(&self) -> String {
        if self.path_prefix.is_empty() {
            return self.owner.clone();
        }
        format!("{}/{}", self.path_prefix, self.owner)
    }

    /// The prefix `credential_path` writes under, for error messages.
    fn credential_root(&self) -> String {
        match &self.machine_id {
            Some(id) => format!("{}/{}/mcp/{id}/", self.mount, self.storage_prefix()),
            None => format!("{}/{}/mcp/", self.mount, self.storage_prefix()),
        }
    }

    /// The `bao` command that removes one of this backend's secrets, fully
    /// qualified with `mount` and [`Self::storage_prefix`].
    ///
    /// A [`SecretPath`] is backend-relative on purpose, so it is never enough
    /// on its own to address a command at the operator's shell: the same
    /// relative path lives at a different fully-qualified address in every
    /// mount and subtree. Only the backend that laid the tree out can close
    /// that gap, which is why this lives beside [`Self::storage_prefix`]
    /// rather than being reconstructed wherever a command wants to print one.
    pub fn removal_command(&self, path: &SecretPath) -> String {
        format!("bao kv metadata delete {}/{}/{path}", self.mount, self.storage_prefix())
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
        let read = self
            .exchange(
                reqwest::Method::GET,
                DATA_ROUTE,
                &url,
                path,
                None,
                Absence::IsAMiss,
                |body| {
                    let read: Envelope<ReadPayload> =
                        serde_json::from_value(body).map_err(|e| SecretsError::Malformed {
                            path: path.clone(),
                            cause: format!("response is not a KV v2 read: {e}"),
                            raw: None,
                        })?;

                    // Defensive. A soft-deleted version answers 404 with a null
                    // `data`, which `read_body` already turns into a miss, so this
                    // only catches a 200 shaped the same way.
                    let Some(data) = read.data.data else {
                        return Ok(None);
                    };

                    map_from_json(&data, path).map(Some)
                },
            )
            .await?;

        Ok(read.flatten())
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The existing backend error model stores a display cause with separately redacted payloads; retaining typed sources requires a coordinated error-model migration"
        )
    )]
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
            raw: None,
        })?;

        self.exchange(
            reqwest::Method::POST,
            DATA_ROUTE,
            &url,
            path,
            Some(body),
            Absence::IsAFailure,
            Ok,
        )
        .await?;
        Ok(())
    }

    pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
        let url = self.metadata_url(path)?;
        self.exchange(
            reqwest::Method::DELETE,
            METADATA_ROUTE,
            &url,
            path,
            None,
            Absence::IsAMiss,
            Ok,
        )
        .await?;
        Ok(())
    }

    pub async fn list(&self, prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        let full = match prefix {
            Some(p) => self.full_path(p)?,
            None => self.storage_prefix(),
        };
        let url = format!("{}/v1/{}/metadata/{full}", self.addr, self.mount);
        let anchor = prefix.cloned().unwrap_or_else(|| prefix_anchor(&self.storage_prefix()));

        let method = reqwest::Method::from_bytes(b"LIST").expect("LIST is a valid method token");
        let Some(listing) = self
            .exchange(method, METADATA_ROUTE, &url, &anchor, None, Absence::IsAMiss, |body| {
                let listing: Envelope<ListPayload> =
                    serde_json::from_value(body).map_err(|e| SecretsError::Malformed {
                        path: anchor.clone(),
                        cause: format!("response is not a KV v2 listing: {e}"),
                        raw: None,
                    })?;
                Ok(listing)
            })
            .await?
        else {
            return Ok(Vec::new());
        };

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

    /// The prefix-joined path, checked against what is safe to put in a URL.
    ///
    /// [`SecretPath`] accepts anything a Keychain account attribute accepts,
    /// including spaces and non-ASCII, so the narrower rule is applied here at
    /// the backend that needs it rather than being imposed on every backend.
    fn full_path(&self, path: &SecretPath) -> Result<String, SecretsError> {
        let full = path.with_prefix(&self.storage_prefix());
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
    fn read_token(&self) -> Result<SecretString, TokenError> {
        match &self.token {
            TokenSource::Var(source) => Ok(SecretString::from(
                source.resolve_bootstrap(&FetchedSecrets::new())?.trim().to_string(),
            )),
            TokenSource::File(path) => read_token_file(path),
        }
    }

    /// One request and its classified answer, under an HTTP client span that
    /// covers reading the body, validating it against the KV v2 shape the
    /// caller expects, and the round trip itself. `validate` runs inside the
    /// span so a 200 with an invalid envelope or a non-string secret value
    /// finishes the span failed rather than succeeding before the caller
    /// discovers the problem.
    #[allow(clippy::too_many_arguments)]
    async fn exchange<T>(
        &self,
        method: reqwest::Method,
        route: UrlTemplate,
        url: &str,
        path: &SecretPath,
        body: Option<Value>,
        absence: Absence,
        validate: impl FnOnce(Value) -> Result<T, SecretsError>,
    ) -> Result<Option<T>, SecretsError> {
        let call = HttpSpan::start(BackendKind::Openbao, &method, route, &self.server);
        let mut answered = None;
        let result = async {
            let response = self.send(method, url, path, body).await?;
            answered = Some(response.status());
            call.responded(response.status());
            match self.read_body(response, path, absence).await? {
                Some(body) => validate(body).map(Some),
                None => Ok(None),
            }
        }
        .instrument(call.span().clone())
        .await;
        call.finish(answered, &result);
        result
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
        SecretsError::Transport(format!("OpenBao at {}: {}", self.addr, with_causes(e)))
    }

    /// Classify the response. `Ok(None)` is the "no such secret" case, which
    /// every caller turns into its own empty answer.
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The existing backend error model stores a display cause with separately redacted payloads; retaining typed sources requires a coordinated error-model migration"
        )
    )]
    async fn read_body(
        &self,
        response: reqwest::Response,
        path: &SecretPath,
        absence: Absence,
    ) -> Result<Option<Value>, SecretsError> {
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
                    raw: None,
                });
        }

        let errors = ErrorBody::of(&body);

        match status.as_u16() {
            // Both a missing secret and a missing mount answer 404; only the
            // body separates a miss from a misconfiguration.
            404 if errors.is_empty() => match absence {
                Absence::IsAMiss => Ok(None),
                Absence::IsAFailure => Err(SecretsError::Unavailable(format!(
                    "OpenBao at {} answered `404` with no error to report, so nothing was \
                     stored: check that `{}` is a KV v2 mount and that `addr` names OpenBao \
                     itself rather than something standing in front of it",
                    self.addr, self.mount
                ))),
            },
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
                "OpenBao at {} redirected {other} to a different origin, and the token is not \
                 sent anywhere but `addr`. Point `addr` at the active node or at a load balancer \
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
fn is_addressable_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && segment.bytes().all(is_path_byte)
}

/// The configured instance is the only origin allowed to receive the token,
/// and only a redirect that leaves the request intact is worth following.
///
/// reqwest strips only the headers it knows are sensitive, and `X-Vault-Token`
/// is not one of them, so a followed redirect re-sends the token verbatim. A
/// remote answering 307 with somewhere else is asking for the token, whether or
/// not the target is itself https.
fn only_this_origin(addr: &reqwest::Url) -> reqwest::redirect::Policy {
    let origin = addr.origin();
    reqwest::redirect::Policy::custom(move |attempt| {
        let status = attempt.status();
        if attempt.url().origin() != origin {
            attempt.stop()
        } else if !preserves_the_request(status) {
            attempt.error(format!(
                "answered {status} within its own origin, which would resend the request as a \
                 bodyless GET, so a write would store nothing"
            ))
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// The error and everything under it, joined.
///
/// `SecretsError::Transport` carries prose rather than a source chain, and
/// reqwest puts the useful part underneath: a refused redirect displays as
/// "error following redirect for url (...)" and keeps the reason it was refused
/// one level down. Stopping at the top would report that something went wrong
/// without saying what.
fn with_causes(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(next) = cause {
        out.push_str(": ");
        out.push_str(&next.to_string());
        cause = next.source();
    }
    out
}

/// Whether following the redirect would leave the request as it was sent.
///
/// `301`, `302` and `303` are defined to become a bodyless `GET`, and reqwest
/// implements that. A `set` answered with one would come back as the read of an
/// untouched path, so the caller would be told a credential was stored when
/// nothing had been written. `307` and `308` keep the method and the body, and
/// are what OpenBao answers to send a standby to the active node, so refusing
/// the rewriting three costs nothing the cluster relies on.
fn preserves_the_request(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::TEMPORARY_REDIRECT | reqwest::StatusCode::PERMANENT_REDIRECT
    )
}

/// The roots this backend trusts, which are the OS roots until a
/// `ca_cert_file` says otherwise.
///
/// A pinned deployment stays pinned: `tls_certs_only` replaces the OS roots
/// rather than merging with them, so a backend given a bundle trusts that
/// bundle and nothing else.
fn trusting_only(
    builder: reqwest::ClientBuilder,
    ca_cert_file: Option<&Path>,
) -> Result<reqwest::ClientBuilder, OpenBaoBuildError> {
    let Some(path) = ca_cert_file else {
        return Ok(builder);
    };
    let pem = std::fs::read(path).map_err(|cause| OpenBaoBuildError::CaCertRead {
        path: path.to_path_buf(),
        cause,
    })?;
    let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|cause| OpenBaoBuildError::CaCertParse {
        path: path.to_path_buf(),
        cause,
    })?;
    // Anything without a PEM frame parses as a bundle of nothing rather than
    // as an error, so a `ca_cert_file` naming the wrong file would otherwise
    // build a client that trusts no root at all and fails only at the
    // handshake, far from the setting that caused it.
    if certs.is_empty() {
        return Err(OpenBaoBuildError::CaCertEmpty {
            path: path.to_path_buf(),
        });
    }
    Ok(builder.tls_certs_only(certs))
}

/// `list` with no prefix has no path to blame in an error, so the backend's
/// own prefix stands in for one.
fn prefix_anchor(prefix: &str) -> SecretPath {
    SecretPath::parse(if prefix.is_empty() { "." } else { prefix })
        .unwrap_or_else(|_| SecretPath::parse("openbao").expect("static path is valid"))
}

fn check_segment(field: &'static str, value: &str) -> Result<(), OpenBaoBuildError> {
    if is_addressable_segment(value) {
        Ok(())
    } else {
        Err(OpenBaoBuildError::Segment {
            field,
            value: value.to_string(),
        })
    }
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
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "The existing backend error model stores a display cause with separately redacted payloads; retaining typed sources requires a coordinated error-model migration"
    )
)]
fn map_from_json(object: &Map<String, Value>, path: &SecretPath) -> Result<SecretMap, SecretsError> {
    // No `raw`: OpenBao credential storage arrived in #77, after #73 had
    // already moved credentials into a keyed map, so no path here has ever
    // held a pre-map blob for a caller to retry. Carrying the payload would
    // only put secret values somewhere they are not needed.
    let mut map = SecretMap::new();
    for (name, value) in object {
        let text = value.as_str().ok_or_else(|| SecretsError::Malformed {
            path: path.clone(),
            cause: format!("key `{name}` is not a string; `trg` stores only string values"),
            raw: None,
        })?;
        let key = SecretKey::parse(name).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: e.to_string(),
            raw: None,
        })?;
        map.insert(key, SecretString::from(text.to_string()));
    }
    Ok(map)
}

/// Read a token file, refusing one any other user can read.
///
/// `bao login` writes `~/.vault-token` with mode 600. A file that has been
/// loosened is a live credential leak, and silently using it would hide that.
fn read_token_file(path: &Path) -> Result<SecretString, TokenError> {
    let unreadable = |source| TokenError::Unreadable {
        path: path.to_path_buf(),
        source,
    };

    let meta = std::fs::metadata(path).map_err(unreadable)?;

    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(TokenError::LooseMode {
            path: path.to_path_buf(),
            mode,
        });
    }

    let raw = std::fs::read_to_string(path).map_err(unreadable)?;

    let token = raw.trim();
    if token.is_empty() {
        return Err(TokenError::Empty {
            path: path.to_path_buf(),
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
#[path = "openbao_tests.rs"]
mod tests;
