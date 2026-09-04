//! Secret storage backends, addressed by name.
//!
//! A backend speaks opaque key-value: a [`SecretPath`] addresses a map, and a
//! [`SecretKey`] addresses one value inside that map. Nothing here knows about
//! OAuth. The adapter in [`crate::oauth::store`] is what projects rmcp's
//! `StoredCredentials` onto this model.
//!
//! Dispatch is an enum rather than `dyn`, so no call boxes a future or goes
//! through a vtable. Adding a kind is a variant plus an arm, and the compiler
//! then names every site that needs updating.

pub mod config;
pub mod keychain;
pub mod openbao;

use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};

pub use config::{BackendConfig, BackendError, Registry, SecretsSection, ServerBackendError};
pub use keychain::KeychainBackend;
pub use openbao::OpenBaoBackend;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("no secret stored at `{0}`")]
    NotFound(SecretPath),

    #[error("not authorized to read `{path}`: {cause}")]
    Unauthorized { path: SecretPath, cause: String },

    #[error("secrets backend unavailable: {0}")]
    Unavailable(String),

    #[error("secrets backend transport: {0}")]
    Transport(String),

    #[error("malformed payload at `{path}`: {cause}")]
    Malformed { path: SecretPath, cause: String },

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("`{op}` is not supported by the `{kind}` backend")]
    Unsupported { kind: &'static str, op: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialPathError {
    #[error("server name is not usable as a secret path: {0}")]
    Path(#[from] PathError),

    #[error("server name `{name}` cannot address the `{kind}` backend: {reason}")]
    Name {
        name: String,
        kind: &'static str,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("secret path must not be empty")]
    Empty,

    #[error("secret path `{0}` must not start or end with `/`")]
    Slash(String),

    #[error("secret path `{0}` must not contain an empty segment")]
    EmptySegment(String),

    #[error("secret path `{0}` must not contain a `.` or `..` segment")]
    DotSegment(String),

    #[error("secret path `{0}` must not contain `#` (that separates a path from a key)")]
    Hash(String),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("secret key must not be empty")]
    Empty,

    #[error("secret key `{0}` must not contain `#`")]
    Hash(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error("secret reference `{0}` must be `<path>#<key>`")]
    Shape(String),

    #[error("secret reference `{reference}`: {cause}")]
    Path { reference: String, cause: PathError },

    #[error("secret reference `{reference}`: {cause}")]
    Key { reference: String, cause: KeyError },
}

/// A validated path addressing one map of secrets.
///
/// Structural validation only. A path is not a secret, so it prints in full.
/// Character-set restrictions belong at the backend boundary that needs them:
/// the Keychain takes an arbitrary opaque account string, so anything the user
/// can name a server is legal here.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretPath(String);

impl SecretPath {
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        if raw.contains('#') {
            return Err(PathError::Hash(raw.to_string()));
        }
        if raw.starts_with('/') || raw.ends_with('/') {
            return Err(PathError::Slash(raw.to_string()));
        }
        for segment in raw.split('/') {
            if segment.is_empty() {
                return Err(PathError::EmptySegment(raw.to_string()));
            }
            if segment == "." || segment == ".." {
                return Err(PathError::DotSegment(raw.to_string()));
            }
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Join a backend-owned prefix onto this path.
    ///
    /// The prefix lives on the backend, not on the path, because the same
    /// logical reference can address two backends with different prefixes.
    pub fn with_prefix(&self, prefix: &str) -> String {
        if prefix.is_empty() {
            self.0.clone()
        } else {
            format!("{prefix}/{}", self.0)
        }
    }
}

impl fmt::Display for SecretPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SecretPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretPath({:?})", self.0)
    }
}

/// A single key within the map stored at a [`SecretPath`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretKey(String);

impl SecretKey {
    pub fn parse(raw: &str) -> Result<Self, KeyError> {
        if raw.is_empty() {
            return Err(KeyError::Empty);
        }
        if raw.contains('#') {
            return Err(KeyError::Hash(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({:?})", self.0)
    }
}

/// The parsed `"<path>#<key>"` form used by config references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    path: SecretPath,
    key: SecretKey,
}

impl SecretRef {
    pub fn parse(raw: &str) -> Result<Self, RefError> {
        let mut parts = raw.splitn(2, '#');
        let (Some(path), Some(key)) = (parts.next(), parts.next()) else {
            return Err(RefError::Shape(raw.to_string()));
        };
        if key.contains('#') {
            return Err(RefError::Shape(raw.to_string()));
        }
        let path = SecretPath::parse(path).map_err(|cause| RefError::Path {
            reference: raw.to_string(),
            cause,
        })?;
        let key = SecretKey::parse(key).map_err(|cause| RefError::Key {
            reference: raw.to_string(),
            cause,
        })?;
        Ok(Self { path, key })
    }

    pub fn path(&self) -> &SecretPath {
        &self.path
    }

    pub fn key(&self) -> &SecretKey {
        &self.key
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.path, self.key)
    }
}

/// The map stored at one [`SecretPath`].
///
/// `Debug` prints key names and redacts every value, and there is deliberately
/// no `Display`, so a stray `{:?}` in a log line cannot leak a secret.
#[derive(Clone, Default)]
pub struct SecretMap(HashMap<SecretKey, SecretString>);

impl SecretMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn insert(&mut self, key: SecretKey, value: SecretString) {
        self.0.insert(key, value);
    }

    pub fn remove(&mut self, key: &SecretKey) -> Option<SecretString> {
        self.0.remove(key)
    }

    pub fn get(&self, key: &SecretKey) -> Option<&SecretString> {
        self.0.get(key)
    }

    pub fn contains_key(&self, key: &SecretKey) -> bool {
        self.0.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Key names in sorted order, so output and tests are deterministic.
    pub fn sorted_keys(&self) -> Vec<&SecretKey> {
        let mut keys: Vec<_> = self.0.keys().collect();
        keys.sort();
        keys
    }

    /// Encode as a flat JSON object. This is the on-disk payload for backends
    /// that store one opaque blob per path, such as the Keychain.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let plain: HashMap<&str, &str> = self.0.iter().map(|(k, v)| (k.as_str(), v.expose_secret())).collect();
        serde_json::to_string(&plain)
    }

    /// Decode the payload written by [`SecretMap::to_json`].
    pub fn from_json(raw: &str) -> Result<Self, serde_json::Error> {
        let plain: HashMap<String, String> = serde_json::from_str(raw)?;
        let mut map = Self::new();
        for (k, v) in plain {
            // A key that round-trips out of storage was validated on the way
            // in; anything unparseable is a corrupt payload, not a new key.
            if let Ok(key) = SecretKey::parse(&k) {
                map.insert(key, SecretString::from(v));
            }
        }
        Ok(map)
    }
}

impl fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("SecretMap");
        for key in self.sorted_keys() {
            out.field(key.as_str(), &"<redacted>");
        }
        out.finish()
    }
}

/// Every backend kind, dispatched statically.
#[derive(Clone)]
pub enum Backend {
    Keychain(KeychainBackend),
    OpenBao(OpenBaoBackend),
    #[cfg(test)]
    Fake(fake::FakeBackend),
}

impl Backend {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Keychain(_) => "keychain",
            Self::OpenBao(_) => "openbao",
            #[cfg(test)]
            Self::Fake(_) => "fake",
        }
    }

    /// User-facing description of where this backend puts things, for command
    /// output that used to hardcode "macOS Keychain".
    pub fn describe(&self) -> String {
        match self {
            Self::Keychain(b) => format!("the macOS Keychain (service `{}`)", b.service()),
            Self::OpenBao(b) => format!("OpenBao at {} (mount `{}`)", b.addr(), b.mount()),
            #[cfg(test)]
            Self::Fake(_) => "an in-memory fake".to_string(),
        }
    }

    /// Where this backend keeps the OAuth credentials for one MCP server.
    ///
    /// The layout is the backend's own concern: the Keychain is already scoped
    /// to one machine and one login keychain, so it keeps addressing items by
    /// bare server name and existing items stay readable. OpenBao is shared
    /// across machines, so it scopes the path per machine.
    pub fn credential_path(&self, server: &str) -> Result<SecretPath, CredentialPathError> {
        match self {
            Self::OpenBao(b) => {
                b.check_server_name(server)
                    .map_err(|reason| CredentialPathError::Name {
                        name: server.to_string(),
                        kind: "openbao",
                        reason,
                    })?;
                Ok(SecretPath::parse(&b.credential_path(server))?)
            }
            Self::Keychain(_) => Ok(SecretPath::parse(server)?),
            #[cfg(test)]
            Self::Fake(_) => Ok(SecretPath::parse(server)?),
        }
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        match self {
            Self::Keychain(b) => b.get(path).await,
            Self::OpenBao(b) => b.get(path).await,
            #[cfg(test)]
            Self::Fake(b) => b.get(path).await,
        }
    }

    pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
        match self {
            Self::Keychain(b) => b.set(path, map).await,
            Self::OpenBao(b) => b.set(path, map).await,
            #[cfg(test)]
            Self::Fake(b) => b.set(path, map).await,
        }
    }

    pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
        match self {
            Self::Keychain(b) => b.delete(path).await,
            Self::OpenBao(b) => b.delete(path).await,
            #[cfg(test)]
            Self::Fake(b) => b.delete(path).await,
        }
    }

    pub async fn list(&self, prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        match self {
            Self::Keychain(b) => b.list(prefix).await,
            Self::OpenBao(b) => b.list(prefix).await,
            #[cfg(test)]
            Self::Fake(b) => b.list(prefix).await,
        }
    }
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Backend({})", self.kind())
    }
}

#[cfg(test)]
pub use fake::FakeFailure;

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Read failures a test can inject, so that callers which must distinguish
    /// "unreadable" from "unreachable" can be exercised.
    #[derive(Clone, Copy, Debug)]
    pub enum FakeFailure {
        Transport,
        Malformed,
    }

    /// In-memory backend for unit tests.
    ///
    /// `#[cfg(test)]`, so it costs nothing in a release build. Integration
    /// tests under `tests/` compile this crate without `cfg(test)` and cannot
    /// see it; those drive the CLI against a real backend instead.
    #[derive(Clone, Default)]
    pub struct FakeBackend {
        entries: Arc<Mutex<HashMap<SecretPath, SecretMap>>>,
        get_failure: Arc<Mutex<Option<FakeFailure>>>,
    }

    impl FakeBackend {
        pub fn new() -> Self {
            Self::default()
        }

        /// Make every subsequent `get` fail until cleared with `None`.
        pub fn set_get_failure(&self, failure: Option<FakeFailure>) {
            *self.get_failure.lock().expect("fake backend lock") = failure;
        }

        pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
            match *self.get_failure.lock().expect("fake backend lock") {
                Some(FakeFailure::Transport) => return Err(SecretsError::Transport("injected".to_string())),
                Some(FakeFailure::Malformed) => {
                    return Err(SecretsError::Malformed {
                        path: path.clone(),
                        cause: "injected".to_string(),
                    })
                }
                None => {}
            }
            Ok(self.entries.lock().expect("fake backend lock").get(path).cloned())
        }

        pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
            self.entries
                .lock()
                .expect("fake backend lock")
                .insert(path.clone(), map.clone());
            Ok(())
        }

        pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
            self.entries.lock().expect("fake backend lock").remove(path);
            Ok(())
        }

        pub async fn list(&self, prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
            let entries = self.entries.lock().expect("fake backend lock");
            let mut out: Vec<String> = entries
                .keys()
                .filter(|p| match prefix {
                    Some(prefix) => p.as_str().starts_with(prefix.as_str()),
                    None => true,
                })
                .map(|p| p.as_str().to_string())
                .collect();
            out.sort();
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rejects_structural_problems() {
        assert!(matches!(SecretPath::parse(""), Err(PathError::Empty)));
        assert!(matches!(SecretPath::parse("/a"), Err(PathError::Slash(_))));
        assert!(matches!(SecretPath::parse("a/"), Err(PathError::Slash(_))));
        assert!(matches!(SecretPath::parse("a//b"), Err(PathError::EmptySegment(_))));
        assert!(matches!(SecretPath::parse("a/../b"), Err(PathError::DotSegment(_))));
        assert!(matches!(SecretPath::parse("a/./b"), Err(PathError::DotSegment(_))));
        assert!(matches!(SecretPath::parse("a#b"), Err(PathError::Hash(_))));
    }

    #[test]
    fn path_accepts_what_a_server_name_can_be() {
        // Server names are TOML table keys today, so spaces and dots are legal
        // and Phase 1 must not start rejecting them.
        for raw in ["github", "my server", "a.b", "mcp/github", "Ünïcode"] {
            assert!(SecretPath::parse(raw).is_ok(), "should accept {raw:?}");
        }
    }

    #[test]
    fn path_prefix_join() {
        let p = SecretPath::parse("mcp/github").expect("parse");
        assert_eq!(p.with_prefix("trg"), "trg/mcp/github");
        assert_eq!(p.with_prefix(""), "mcp/github");
    }

    #[test]
    fn key_rejects_empty_and_hash() {
        assert!(matches!(SecretKey::parse(""), Err(KeyError::Empty)));
        assert!(matches!(SecretKey::parse("a#b"), Err(KeyError::Hash(_))));
        assert!(SecretKey::parse("token").is_ok());
    }

    #[test]
    fn secret_ref_parses_path_and_key() {
        let r = SecretRef::parse("mcp/github#token").expect("parse");
        assert_eq!(r.path().as_str(), "mcp/github");
        assert_eq!(r.key().as_str(), "token");
        assert_eq!(r.to_string(), "mcp/github#token");
    }

    #[test]
    fn secret_ref_rejects_bad_shapes() {
        assert!(matches!(SecretRef::parse("nohash"), Err(RefError::Shape(_))));
        assert!(matches!(SecretRef::parse("a#b#c"), Err(RefError::Shape(_))));
        assert!(matches!(SecretRef::parse("#key"), Err(RefError::Path { .. })));
        assert!(matches!(SecretRef::parse("path#"), Err(RefError::Key { .. })));
    }

    #[test]
    fn secret_map_debug_redacts_every_value() {
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse("token").unwrap(), SecretString::from("hunter2"));
        map.insert(SecretKey::parse("other").unwrap(), SecretString::from("s3cret"));

        let rendered = format!("{map:?}");
        assert!(!rendered.contains("hunter2"), "leaked a value: {rendered}");
        assert!(!rendered.contains("s3cret"), "leaked a value: {rendered}");
        assert!(rendered.contains("token"), "should name keys: {rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn secret_map_json_roundtrip() {
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse("a").unwrap(), SecretString::from("1"));
        map.insert(SecretKey::parse("b").unwrap(), SecretString::from("2"));

        let json = map.to_json().expect("encode");
        let back = SecretMap::from_json(&json).expect("decode");

        assert_eq!(back.len(), 2);
        assert_eq!(
            back.get(&SecretKey::parse("a").unwrap()).map(|v| v.expose_secret()),
            Some("1")
        );
        assert_eq!(
            back.get(&SecretKey::parse("b").unwrap()).map(|v| v.expose_secret()),
            Some("2")
        );
    }

    #[test]
    fn a_keychain_credential_path_is_the_server_name_verbatim() {
        let backend = Backend::Keychain(KeychainBackend::with_default_service());
        assert_eq!(backend.credential_path("github").expect("path").as_str(), "github");
    }

    #[test]
    fn an_openbao_credential_path_is_scoped_per_machine() {
        let backend = Backend::OpenBao(
            openbao::OpenBaoBackend::new(openbao::OpenBaoSettings {
                addr: "http://bao.example.com:8200".to_string(),
                mount: "secret".to_string(),
                path_prefix: "trg".to_string(),
                machine_id: "laptop".to_string(),
                token: openbao::TokenSource::Var(crate::config::VarSource::Literal("t".to_string())),
                ca_cert_file: None,
                timeout: std::time::Duration::from_secs(5),
            })
            .expect("build"),
        );

        assert_eq!(
            backend.credential_path("github").expect("path").as_str(),
            "mcp/laptop/github"
        );

        let err = backend.credential_path("my server").expect_err("should refuse");
        assert!(err.to_string().contains("openbao"), "{err}");
    }

    #[tokio::test]
    async fn fake_backend_roundtrips_through_the_enum() {
        let backend = Backend::Fake(fake::FakeBackend::new());
        let path = SecretPath::parse("mcp/github").expect("parse");
        assert_eq!(backend.kind(), "fake");
        assert!(backend.get(&path).await.expect("get").is_none());

        let mut map = SecretMap::new();
        map.insert(SecretKey::parse("token").unwrap(), SecretString::from("v"));
        backend.set(&path, &map).await.expect("set");

        let loaded = backend.get(&path).await.expect("get").expect("some");
        assert_eq!(
            loaded
                .get(&SecretKey::parse("token").unwrap())
                .map(|v| v.expose_secret()),
            Some("v")
        );

        backend.delete(&path).await.expect("delete");
        assert!(backend.get(&path).await.expect("get").is_none());
    }
}
