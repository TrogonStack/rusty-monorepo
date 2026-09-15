//! Adapter from rmcp's `CredentialStore` onto a [`Backend`].
//!
//! rmcp owns the `StoredCredentials` shape and takes ownership of the store, so
//! this is the one place left that needs `#[async_trait]`: the trait is rmcp's,
//! and it must stay dyn-compatible for them. Everything below it dispatches
//! statically.
//!
//! A whole `StoredCredentials` serializes into one key ([`CREDENTIALS_KEY_V2`])
//! of the map stored at the server's [`SecretPath`], which keeps the backend
//! ignorant of OAuth and leaves room for other keys at the same path later.
//! The key is versioned because this crate, not rmcp, owns the envelope
//! around that blob: `load` recognizes two older shapes it can still read and
//! rewrites either one onto [`CREDENTIALS_KEY_V2`] the moment it sees it, so
//! nothing downstream of here ever has to know they existed.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use secrecy::{ExposeSecret, SecretString};

use crate::secrets::{Backend, SecretKey, SecretMap, SecretPath, SecretsError};
use crate::shell::quote_for_shell;

/// The legacy, unversioned key. `save` no longer writes it, but `load` still
/// reads it once, to migrate whatever it finds onto [`CREDENTIALS_KEY_V2`].
pub const CREDENTIALS_KEY: &str = "credentials";

/// The key under which a server's OAuth credentials live today.
///
/// Versions this envelope, not rmcp's `StoredCredentials` shape: the value
/// stored under this key is whatever `serde_json::to_string` produces for
/// rmcp's type, which is unversioned and out of this crate's control. What
/// `trg` owns, and versions here, is the choice to keep that blob under one
/// key at all rather than splitting it across several, so a decode failure
/// can name which envelope shape it hit.
pub const CREDENTIALS_KEY_V2: &str = "credentials.v2";

/// Everything `load`'s read ladder can conclude once it has already tried
/// `credentials.v2`, the unversioned wrapper, and the bare pre-#73 blob.
///
/// Kept apart from [`SecretsError`], which the backend layer owns and which
/// therefore stays ignorant of what an OAuth credential looks like.
#[derive(Debug, thiserror::Error)]
enum CredentialReadError {
    /// Every shape `load` knows failed to parse. A `serde_json` decode error
    /// would say this too, but with a line and column a person cannot act
    /// on; this names the actual situation and its fix instead.
    #[error(
        "no OAuth credentials could be read at `{path}`: the stored payload does not match any \
         shape this trg understands"
    )]
    Corrupt { path: SecretPath },

    /// A key shaped like `credentials.vN`, for an `N` this trg does not
    /// recognize, is the only credential key present. That is a newer `trg`
    /// sharing this backend, not a corrupt payload.
    #[error(
        "OAuth credentials at `{path}` were written under `{key}`, a version this trg does not \
         understand yet; upgrade trg to read them"
    )]
    NewerVersion { path: SecretPath, key: String },
}

/// Where the store leaves what it was actually told, on the way past rmcp.
///
/// [`CredentialStore`] may only answer with rmcp's [`AuthError`], whose one
/// variant able to carry a storage failure renders as `Internal error: ...`.
/// That reads like a bug in `trg` rather than the expired token it usually is,
/// so the message written here is what the caller reports instead.
///
/// Cloning shares the slot, which is the point: the store itself is moved into
/// rmcp and never seen again.
#[derive(Clone, Default)]
pub struct StorageFailure(Arc<Mutex<Option<String>>>);

impl StorageFailure {
    fn record(&self, message: &str) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(message.to_string());
        }
    }

    /// The last failure, if the store reached the backend and was refused.
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// Where the most recent `load` actually found credentials, when that was
/// not `path`.
///
/// Modelled on [`StorageFailure`] for the same reason: the store is moved
/// into rmcp and never seen again, so this is the only way a caller learns a
/// load fell through to the pre-`machine_id` shared path rather than the
/// machine-scoped one it asked for.
#[derive(Clone, Default)]
pub struct FallbackRead(Arc<Mutex<Option<SecretPath>>>);

impl FallbackRead {
    fn record(&self, path: Option<SecretPath>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = path;
        }
    }

    /// The shared path the most recent `load` actually read from, if it fell
    /// back to one.
    pub fn get(&self) -> Option<SecretPath> {
        self.0.lock().ok().and_then(|slot| slot.clone())
    }
}

pub struct OAuthCredentialStore {
    backend: Backend,
    path: SecretPath,
    /// The pre-`machine_id` shared path to read when `path` is empty.
    /// `load` never copies, moves, or deletes what it finds here. `save`
    /// does write here, but only when this is where the credential being
    /// refreshed was last read from: that refreshes the shared grant in
    /// place, the same way it worked before `machine_id` existed, rather
    /// than copying it onto the machine-scoped path, which would leave both
    /// paths refreshing the same grant, the replay `machine_id` exists to
    /// prevent.
    fallback: Option<SecretPath>,
    /// The `--server` name recovery advice has to quote. Not recoverable from
    /// `path`: only the Keychain stores a server under its bare name, and
    /// OpenBao stores it under `mcp/<machine_id>/<server>`.
    server: String,
    failure: StorageFailure,
    read_from: FallbackRead,
}

impl OAuthCredentialStore {
    pub fn new(backend: Backend, path: SecretPath, server: impl Into<String>, fallback: Option<SecretPath>) -> Self {
        Self {
            backend,
            path,
            fallback,
            server: server.into(),
            failure: StorageFailure::default(),
            read_from: FallbackRead::default(),
        }
    }

    /// A handle on the slot, to be kept before the store is handed to rmcp.
    pub fn failure(&self) -> StorageFailure {
        self.failure.clone()
    }

    /// A handle on the slot, to be kept before the store is handed to rmcp.
    pub fn read_from(&self) -> FallbackRead {
        self.read_from.clone()
    }

    /// Whether the pre-`machine_id` shared path still holds something, for a
    /// caller that has to answer whether this machine is still effectively
    /// signed in, such as `logout` right after it clears the machine-scoped
    /// path.
    ///
    /// Deliberately not [`Self::load_from_fallback`]: that method turns a
    /// fallback read error into `Ok(None)`, which is right for a courtesy
    /// read that must not block a login, but wrong here. `logout` needs to
    /// know, and a storage error means it does not, so this propagates one
    /// instead of reporting a clean logout it cannot vouch for. A payload
    /// that is present but will not decode still counts as reachable, not as
    /// clean: erring toward telling the operator something is there is the
    /// safe direction.
    pub async fn fallback_is_reachable(&self) -> Result<Option<SecretPath>, AuthError> {
        let Some(fallback) = self.fallback.clone() else {
            return Ok(None);
        };
        match self.backend.get(&fallback).await {
            Ok(Some(_)) => Ok(Some(fallback)),
            Ok(None) => Ok(None),
            Err(e) => Err(self.to_auth_error(e)),
        }
    }

    fn key() -> SecretKey {
        SecretKey::parse(CREDENTIALS_KEY).expect("CREDENTIALS_KEY is a valid secret key")
    }

    fn key_v2() -> SecretKey {
        SecretKey::parse(CREDENTIALS_KEY_V2).expect("CREDENTIALS_KEY_V2 is a valid secret key")
    }
}

#[async_trait]
impl CredentialStore for OAuthCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // Reset up front so a store reused across more than one `load` never
        // reports the previous call's fallback hit as this one's.
        self.read_from.record(None);
        match self.backend.get(&self.path).await {
            Ok(None) => self.load_from_fallback().await,
            Ok(Some(map)) => self.load_from_map(map).await,
            // The outer map itself would not decode: what a payload written
            // before credentials lived in a keyed map looks like. `raw` is
            // only `Some` when the backend that hit this had the exact text
            // on hand to retry under that older shape.
            Err(SecretsError::Malformed { raw, .. }) => self.load_legacy_v1(raw).await,
            Err(err) => Err(self.to_auth_error(err)),
        }
    }

    /// Writes back to wherever the credential being refreshed actually came
    /// from: `self.path` when the last `load` hit it (or there was no prior
    /// `load`), the fallback when the last `load` fell through to it. rmcp
    /// runs every token refresh through `save`, so targeting `self.path`
    /// unconditionally would copy a fallback-origin credential onto the
    /// machine-scoped path on its first refresh, leaving two paths
    /// refreshing the same grant, the replay `machine_id` exists to prevent.
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::InternalError(format!("encode credentials: {e}")))?;
        let target = self.read_from.get().unwrap_or_else(|| self.path.clone());

        let mut map = match self.backend.get(&target).await {
            Ok(existing) => existing.unwrap_or_default(),
            // An outer payload that will not decode leaves nothing structured
            // behind it, whether that is a pre-#73 raw `StoredCredentials`
            // blob (see `load`) or some other shape this trg cannot parse a
            // map from. Either way there are no sibling keys to lose by
            // starting fresh.
            Err(SecretsError::Malformed { .. }) => SecretMap::new(),
            // Anything else is transient. Writing through it would replace the
            // whole map with just this key and take any siblings with it.
            Err(e) => return Err(self.to_auth_error(e)),
        };
        map.remove(&Self::key());
        map.insert(Self::key_v2(), SecretString::from(json));

        self.backend.set(&target, &map).await.map_err(|e| self.to_auth_error(e))
    }

    /// Always deletes `self.path`, never the fallback: deleting the shared
    /// path would sign out every other machine still reading from it. A
    /// caller that needs to know whether this machine is still effectively
    /// signed in afterward (because the fallback still holds a credential)
    /// has to check with a subsequent `load`; `clear` itself does not report
    /// it.
    async fn clear(&self) -> Result<(), AuthError> {
        let existing = match self.backend.get(&self.path).await {
            Ok(existing) => existing,
            // `logout` is the documented recovery for an unreadable payload, so
            // it has to drop the path rather than refuse.
            Err(SecretsError::Malformed { .. }) => None,
            Err(e) => return Err(self.to_auth_error(e)),
        };
        let Some(mut map) = existing else {
            return self.backend.delete(&self.path).await.map_err(|e| self.to_auth_error(e));
        };
        map.remove(&Self::key());
        map.remove(&Self::key_v2());
        if map.is_empty() {
            self.backend.delete(&self.path).await.map_err(|e| self.to_auth_error(e))
        } else {
            self.backend
                .set(&self.path, &map)
                .await
                .map_err(|e| self.to_auth_error(e))
        }
    }
}

impl OAuthCredentialStore {
    /// `path` holds nothing. Before answering `None`, check whether the
    /// credential is still sitting at the pre-`machine_id` shared path: it is
    /// safe to read from there, but never to copy, move, or delete, so a hit
    /// here is reported rather than acted on.
    ///
    /// Deliberately does not reuse [`Self::load_from_map`]: that method
    /// migrates an unversioned key by writing `credentials.v2` back to
    /// `self.path`, and doing that from a fallback read would silently copy
    /// the shared credential onto the machine-scoped path, exactly what the
    /// read-only rule forbids.
    async fn load_from_fallback(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(fallback) = self.fallback.clone() else {
            return Ok(None);
        };

        let map = match self.backend.get(&fallback).await {
            Ok(Some(map)) => map,
            Ok(None) => return Ok(None),
            // A read error at the fallback is not a reason to refuse a login
            // that would otherwise proceed normally against an empty primary
            // path: the fallback is a courtesy, not a dependency.
            Err(e) => {
                tracing::debug!(
                    path = %fallback,
                    error = %e,
                    "could not read the shared OAuth credential fallback path"
                );
                return Ok(None);
            }
        };

        let Some(credentials) = self.decode_from_fallback_map(&fallback, &map)? else {
            return Ok(None);
        };

        self.read_from.record(Some(fallback.clone()));
        tracing::warn!(
            server = %self.server,
            primary = %self.path,
            fallback = %fallback,
            "read OAuth credentials for `{}` from the pre-machine_id shared path `{}`; they are \
             still stored there and still readable by any other machine sharing it. Run `trg mcp \
             auth login --server {}` to write them to the machine-scoped path `{}`.",
            self.server, fallback, self.server, self.path
        );
        Ok(Some(credentials))
    }

    /// Decode whatever credential value a fallback read turned up, without
    /// writing anything back: the fallback path stays exactly as found,
    /// migrated key shape and all.
    ///
    /// Applies the same corruption and newer-version handling
    /// [`Self::load_from_map`] applies to the primary path: a fallback that
    /// holds something this trg cannot read is not the same as a fallback
    /// that holds nothing, and reporting it as absent would send `load` on
    /// to open a fresh authorization flow against a path that in fact has a
    /// credential on it. A genuinely empty fallback still answers `Ok(None)`.
    fn decode_from_fallback_map(
        &self,
        path: &SecretPath,
        map: &SecretMap,
    ) -> Result<Option<StoredCredentials>, AuthError> {
        if let Some(value) = map.get(&Self::key_v2()) {
            return self.decode_or_corrupt_at(path, value.expose_secret()).map(Some);
        }
        if let Some(value) = map.get(&Self::key()) {
            return self.decode_or_corrupt_at(path, value.expose_secret()).map(Some);
        }
        if let Some(key) = newer_version_key(map) {
            return Err(self.credential_read_error(CredentialReadError::NewerVersion {
                path: path.clone(),
                key,
            }));
        }
        Ok(None)
    }

    /// Case 2: the outer map decoded. `credentials.v2` wins when present
    /// (today's shape); otherwise the unversioned `credentials` is read and
    /// migrated; otherwise an unrecognized `credentials.vN` is reported as
    /// needing an upgrade; otherwise the path simply holds none of our keys.
    async fn load_from_map(&self, map: SecretMap) -> Result<Option<StoredCredentials>, AuthError> {
        if let Some(value) = map.get(&Self::key_v2()) {
            let raw = value.expose_secret().to_string();
            return self.decode_or_corrupt(&raw).map(Some);
        }

        if let Some(value) = map.get(&Self::key()) {
            let raw = value.expose_secret().to_string();
            let credentials = self.decode_or_corrupt(&raw)?;
            self.migrate_unversioned(map, raw).await;
            return Ok(Some(credentials));
        }

        if let Some(key) = newer_version_key(&map) {
            return Err(self.credential_read_error(CredentialReadError::NewerVersion {
                path: self.path.clone(),
                key,
            }));
        }

        Ok(None)
    }

    /// Case 3: the outer map would not decode at all. `raw` carries the
    /// backend's exact bytes when it has them, which only the Keychain does:
    /// it is the one backend that stored credentials before #73 moved them
    /// into a keyed map. Every other backend answers `None` and this goes
    /// straight to reporting corruption.
    async fn load_legacy_v1(&self, raw: Option<SecretString>) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(raw) = raw else {
            return Err(self.credential_read_error(CredentialReadError::Corrupt {
                path: self.path.clone(),
            }));
        };
        let credentials = self.decode_or_corrupt(raw.expose_secret())?;

        let mut map = SecretMap::new();
        map.insert(Self::key_v2(), raw);
        if let Err(err) = self.backend.set(&self.path, &map).await {
            tracing::warn!(
                path = %self.path,
                error = %err,
                "could not migrate a legacy OAuth credential to `credentials.v2`; will retry on the next load"
            );
        }
        Ok(Some(credentials))
    }

    /// Case 2b's migration: rewrite the same bytes under `credentials.v2`,
    /// dropping the unversioned key, and leave every sibling key untouched.
    /// A failed write is logged and ignored: the credentials just parsed are
    /// valid and usable, and a self-healing migration should retry on the
    /// next load rather than break auth over a storage hiccup.
    async fn migrate_unversioned(&self, mut map: SecretMap, raw: String) {
        map.remove(&Self::key());
        map.insert(Self::key_v2(), SecretString::from(raw));
        if let Err(err) = self.backend.set(&self.path, &map).await {
            tracing::warn!(
                path = %self.path,
                error = %err,
                "could not migrate OAuth credentials to `credentials.v2`; will retry on the next load"
            );
        }
    }

    /// Parse `raw` as `StoredCredentials`, discarding the `serde_json` error
    /// (line and column a person cannot act on) in favor of a message naming
    /// what to do about it. The original error still reaches the debug log.
    fn decode_or_corrupt(&self, raw: &str) -> Result<StoredCredentials, AuthError> {
        self.decode_or_corrupt_at(&self.path, raw)
    }

    /// [`Self::decode_or_corrupt`], naming `path` rather than `self.path` in
    /// the error: the fallback path decodes with the same rules as the
    /// primary, but a message pointing at `self.path` would send the reader
    /// to a secret that is not the one actually holding the bad payload.
    fn decode_or_corrupt_at(&self, path: &SecretPath, raw: &str) -> Result<StoredCredentials, AuthError> {
        serde_json::from_str(raw).map_err(|e| {
            tracing::debug!(path = %path, error = %e, "stored OAuth credentials did not decode");
            self.credential_read_error(CredentialReadError::Corrupt { path: path.clone() })
        })
    }

    /// The `logout` then `login` remedy, which every unrecoverable credential
    /// read shares regardless of which failure produced it.
    fn recovery_advice(&self) -> String {
        let server = quote_for_shell(&self.server);
        format!(
            "Run `trg mcp auth logout --server {server}` then `trg mcp auth login --server {server}` to re-authorize"
        )
    }

    fn credential_read_error(&self, err: CredentialReadError) -> AuthError {
        let message = match &err {
            CredentialReadError::Corrupt { .. } => format!("{err}. {}", self.recovery_advice()),
            // Logging out would not help: the payload is fine, this trg is
            // simply too old to read it.
            CredentialReadError::NewerVersion { .. } => err.to_string(),
        };
        self.failure.record(&message);
        AuthError::InternalError(message)
    }

    /// rmcp models every storage failure as `InternalError(String)`, so the
    /// [`SecretsError`] variant survives only in the message. The message is
    /// also left in [`StorageFailure`], which is the copy that reaches a person.
    fn to_auth_error(&self, err: SecretsError) -> AuthError {
        let message = err.to_string();
        self.failure.record(&message);
        AuthError::InternalError(message)
    }
}

/// The full key name of an unrecognized `credentials.vN` entry with `N`
/// greater than the version this trg understands, if the map holds one.
///
/// Anything shaped like this key but not exactly `credentials.v2` was
/// written by a `trg` that speaks a version this one does not; surfacing
/// that is what lets `load` tell "upgrade trg" apart from "these credentials
/// are simply gone".
fn newer_version_key(map: &SecretMap) -> Option<String> {
    const VERSIONED_PREFIX: &str = "credentials.v";
    map.sorted_keys().into_iter().find_map(|key| {
        let raw = key.as_str();
        let version: u64 = raw.strip_prefix(VERSIONED_PREFIX)?.parse().ok()?;
        (version > 2).then(|| raw.to_string())
    })
}

/// A pre-#73 payload as it actually reads in the wild: `token_response`
/// populated rather than `null`, `granted_scopes` non-empty, and `issuer`
/// missing entirely rather than present and `null`, since that field did not
/// exist yet when this shape was written. Token values are obviously fake.
/// Shared by the unit tests below and by `legacy_payload_tests`, the latter
/// of which seeds a real keychain item with it.
#[cfg(test)]
const LEGACY_RAW_PAYLOAD: &str = r#"{"client_id":"legacy-client","token_response":{"access_token":"fake-access-token","token_type":"bearer","expires_in":3600,"refresh_token":"fake-refresh-token","scope":"read write"},"granted_scopes":["read","write"],"token_received_at":1700000000}"#;

#[cfg(test)]
mod tests {
    use oauth2::TokenResponse;

    use super::*;
    use crate::secrets::{fake::FakeBackend, FakeFailure};

    fn store() -> (Backend, OAuthCredentialStore) {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("github").expect("parse");
        (
            backend.clone(),
            OAuthCredentialStore::new(backend, path, "github", None),
        )
    }

    /// Like [`store`], but with an explicit fallback path on the same
    /// backend, for exercising the pre-`machine_id` read-through.
    fn store_with_fallback(fallback: SecretPath) -> (Backend, OAuthCredentialStore) {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("github").expect("parse");
        (
            backend.clone(),
            OAuthCredentialStore::new(backend, path, "github", Some(fallback)),
        )
    }

    fn credentials(client_id: &str) -> StoredCredentials {
        StoredCredentials::new(client_id.to_string(), None, vec!["scope".to_string()], Some(42))
    }

    fn fake(backend: &Backend) -> &FakeBackend {
        let Backend::Fake(fake) = backend else {
            unreachable!("store() builds a fake")
        };
        fake
    }

    #[tokio::test]
    async fn load_fresh_returns_none() {
        let (_, store) = store();
        assert!(store.load().await.expect("load").is_none());
    }

    #[tokio::test]
    async fn save_load_roundtrip() {
        let (_, store) = store();
        store.save(credentials("abc")).await.expect("save");

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "abc");
        assert_eq!(loaded.granted_scopes, vec!["scope".to_string()]);
    }

    #[tokio::test]
    async fn save_twice_overwrites() {
        let (_, store) = store();
        store.save(credentials("first")).await.expect("first");
        store.save(credentials("second")).await.expect("second");

        assert_eq!(store.load().await.expect("load").expect("some").client_id, "second");
    }

    #[tokio::test]
    async fn save_preserves_other_keys_at_the_same_path() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");

        store.save(credentials("abc")).await.expect("save");

        let map = backend.get(&path).await.expect("get").expect("some");
        assert_eq!(
            map.get(&SecretKey::parse("api_key").unwrap())
                .map(|v| v.expose_secret()),
            Some("keep-me")
        );
        assert!(map.contains_key(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap()));
    }

    #[tokio::test]
    async fn save_writes_v2_and_drops_a_stale_unversioned_key() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from("stale".to_string()),
        );
        backend.set(&path, &seed).await.expect("seed");

        store.save(credentials("abc")).await.expect("save");

        let map = backend.get(&path).await.expect("get").expect("some");
        assert!(!map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));
        assert!(map.contains_key(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap()));
    }

    #[tokio::test]
    async fn clear_removes_only_the_credentials_key() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");
        store.save(credentials("abc")).await.expect("save");

        store.clear().await.expect("clear");

        assert!(store.load().await.expect("load").is_none());
        let map = backend.get(&path).await.expect("get").expect("still there");
        assert!(map.contains_key(&SecretKey::parse("api_key").unwrap()));
    }

    #[tokio::test]
    async fn clear_deletes_the_path_when_nothing_else_is_left() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        store.save(credentials("abc")).await.expect("save");

        store.clear().await.expect("clear");

        assert!(backend.get(&path).await.expect("get").is_none());
    }

    /// The rendered message, not the `AuthError` rmcp wraps it in, is what a
    /// person reads, so it has to survive the trip.
    #[tokio::test]
    async fn a_refusal_is_left_behind_without_the_prefix_rmcp_would_add() {
        let (backend, store) = store();
        let failure = store.failure();
        fake(&backend).set_get_failure(Some(FakeFailure::Transport));

        let err = store.load().await.expect_err("a transport failure should not load");

        let left = failure.take().expect("the refusal should have been recorded");
        assert!(!left.starts_with("Internal error"), "{left}");
        assert!(err.to_string().contains(&left), "{err} should carry {left}");
    }

    /// Taking it clears it: a later failure that never reached the backend must
    /// not be reported as this one.
    #[tokio::test]
    async fn a_refusal_is_reported_once() {
        let (backend, store) = store();
        let failure = store.failure();
        fake(&backend).set_get_failure(Some(FakeFailure::Transport));
        store.load().await.expect_err("a transport failure should not load");

        assert!(failure.take().is_some());
        assert!(failure.take().is_none());
    }

    /// A load that simply found nothing is not a refusal, and reporting the
    /// empty slot as one would invent a backend problem.
    #[tokio::test]
    async fn a_load_that_succeeds_leaves_nothing_behind() {
        let (_, store) = store();
        let failure = store.failure();

        store.load().await.expect("load");

        assert!(failure.take().is_none());
    }

    #[tokio::test]
    async fn save_propagates_a_read_failure_rather_than_clobbering_siblings() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");

        fake(&backend).set_get_failure(Some(FakeFailure::Transport));
        store
            .save(credentials("abc"))
            .await
            .expect_err("should not write through a read failure");
        fake(&backend).set_get_failure(None);

        let map = backend.get(&path).await.expect("get").expect("some");
        assert_eq!(
            map.get(&SecretKey::parse("api_key").unwrap())
                .map(|v| v.expose_secret()),
            Some("keep-me")
        );
    }

    #[tokio::test]
    async fn save_overwrites_when_the_existing_payload_is_unreadable() {
        let (backend, store) = store();
        fake(&backend).set_get_failure(Some(FakeFailure::Malformed));
        store
            .save(credentials("abc"))
            .await
            .expect("save over an unreadable payload");
        fake(&backend).set_get_failure(None);

        assert_eq!(store.load().await.expect("load").expect("some").client_id, "abc");
    }

    #[tokio::test]
    async fn clear_propagates_a_read_failure_rather_than_deleting_the_path() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");

        fake(&backend).set_get_failure(Some(FakeFailure::Transport));
        store
            .clear()
            .await
            .expect_err("should not delete through a read failure");
        fake(&backend).set_get_failure(None);

        assert!(backend.get(&path).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn malformed_recovery_command_is_copy_pasteable_for_any_server_name() {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("my server").expect("parse");
        let store = OAuthCredentialStore::new(backend.clone(), path, "my server", None);

        fake(&backend).set_get_failure(Some(FakeFailure::Malformed));

        let rendered = store.load().await.expect_err("malformed").to_string();
        assert!(
            rendered.contains("--server 'my server'"),
            "recovery command must survive copy-paste: {rendered}"
        );
        assert!(!rendered.contains("--server my server"), "{rendered}");
    }

    /// OpenBao stores a server under `mcp/<machine_id>/<server>`, so a recovery
    /// command derived from the path would name something `--server` rejects.
    #[tokio::test]
    async fn malformed_recovery_command_names_the_server_not_its_storage_path() {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("mcp/laptop/github").expect("parse");
        let store = OAuthCredentialStore::new(backend.clone(), path, "github", None);

        fake(&backend).set_get_failure(Some(FakeFailure::Malformed));

        let rendered = store.load().await.expect_err("malformed").to_string();
        assert!(rendered.contains("--server github"), "{rendered}");
        assert!(!rendered.contains("--server mcp/laptop/github"), "{rendered}");
    }

    #[tokio::test]
    async fn clear_is_idempotent() {
        let (_, store) = store();
        store.clear().await.expect("clear empty");
        store.clear().await.expect("clear again");
    }

    #[tokio::test]
    async fn load_reads_the_v2_key_directly() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let json = serde_json::to_string(&credentials("direct")).expect("encode");
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(), SecretString::from(json));
        backend.set(&path, &map).await.expect("seed");

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "direct");
    }

    /// The load-bearing guarantee this fallback exists for: a credential
    /// left at the pre-`machine_id` shared path is still readable, its
    /// origin is reported, and `load` itself never writes anywhere. Whether
    /// a later `save` writes back to the shared path is covered separately,
    /// below.
    #[tokio::test]
    async fn a_shared_credential_load_reads_without_writing_anywhere() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());
        let read_from = store.read_from();

        let json = serde_json::to_string(&credentials("shared-cred")).expect("encode");
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(), SecretString::from(json));
        backend.set(&shared, &map).await.expect("seed the shared path");

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "shared-cred");
        assert_eq!(read_from.get(), Some(shared.clone()));

        let primary = SecretPath::parse("github").expect("parse");
        assert!(
            backend.get(&primary).await.expect("get").is_none(),
            "the fallback hit must not have written anything to the primary path"
        );
        assert!(
            backend.get(&shared).await.expect("get").is_some(),
            "the shared payload must still be sitting where it was found"
        );
    }

    /// The mechanism `trg mcp auth login`'s no-fallback fix depends on: with
    /// no fallback configured, a credential sitting at what would otherwise
    /// be the shared path is invisible to `load`, so `initialize_from_store`
    /// cannot report `AlreadyAuthorized` off it and an explicit login
    /// actually runs the flow instead of quietly no-op'ing.
    #[tokio::test]
    async fn a_store_with_no_fallback_never_sees_a_shared_credential_as_already_there() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let backend = Backend::Fake(FakeBackend::new());

        let json = serde_json::to_string(&credentials("shared-cred")).expect("encode");
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(), SecretString::from(json));
        backend.set(&shared, &map).await.expect("seed the shared path");

        let path = SecretPath::parse("github").expect("parse");
        let store = OAuthCredentialStore::new(backend, path, "github", None);

        assert!(
            store.load().await.expect("load").is_none(),
            "a login-shaped store (no fallback) must not treat the shared path as already authorized"
        );
    }

    /// The other half of the guarantee `ensure_credentials_for` relies on for
    /// a freshly minted grant: with no fallback configured, `save` has no
    /// shared path to target, even when one happens to be populated at what
    /// would otherwise be the fallback address. A fresh grant can only ever
    /// land on the primary path, not the address a fallback-carrying store
    /// would have read from.
    #[tokio::test]
    async fn a_store_with_no_fallback_always_saves_to_the_primary_even_when_a_would_be_fallback_path_is_populated() {
        let would_be_fallback = SecretPath::parse("mcp/github").expect("parse");
        let backend = Backend::Fake(FakeBackend::new());

        let shared_json = serde_json::to_string(&credentials("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(),
            SecretString::from(shared_json.clone()),
        );
        backend
            .set(&would_be_fallback, &shared_map)
            .await
            .expect("seed the would-be fallback path");

        let path = SecretPath::parse("github").expect("parse");
        let store = OAuthCredentialStore::new(backend.clone(), path.clone(), "github", None);

        store.save(credentials("fresh-cred")).await.expect("save");

        let primary = backend.get(&path).await.expect("get").expect("some");
        let raw = primary
            .get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
            .expect("v2 key");
        let decoded: StoredCredentials = serde_json::from_str(raw.expose_secret()).expect("decode");
        assert_eq!(
            decoded.client_id, "fresh-cred",
            "a store with no fallback must save a fresh grant to the primary path"
        );

        let would_be_fallback_after = backend
            .get(&would_be_fallback)
            .await
            .expect("get")
            .expect("still there");
        assert_eq!(
            would_be_fallback_after
                .get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
                .map(|v| v.expose_secret()),
            Some(shared_json.as_str()),
            "a store with no fallback must never touch the would-be fallback path"
        );
    }

    #[tokio::test]
    async fn a_primary_hit_never_reads_the_fallback() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());
        let read_from = store.read_from();

        // A fallback that would error if ever touched, so a read reaching it
        // fails the test loudly instead of the assertion below going unmet
        // for the wrong reason.
        fake(&backend).set_get_failure_at(shared, FakeFailure::Transport);
        store.save(credentials("primary-cred")).await.expect("save");

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "primary-cred");
        assert_eq!(read_from.get(), None);
    }

    /// With no prior `load`, `save` has no recorded origin to write back to,
    /// so it defaults to the primary path, exactly as it did before a
    /// fallback existed.
    #[tokio::test]
    async fn a_save_with_no_prior_load_targets_the_primary_path_leaving_the_shared_payload_untouched() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(),
            SecretString::from(shared_json.clone()),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.save(credentials("primary-cred")).await.expect("save");

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        assert_eq!(
            shared_after
                .get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
                .map(|v| v.expose_secret()),
            Some(shared_json.as_str()),
            "a save with no recorded fallback origin must never touch the shared payload"
        );
    }

    /// The core guarantee behind the fix: rmcp runs every token refresh
    /// through `save`, so a refresh of a credential that was last read from
    /// the shared path must land back on the shared path, not create a
    /// machine-scoped copy. That is what keeps a machine still on the
    /// shared path refreshing it in place, precisely the pre-`machine_id`
    /// behaviour, rather than orphaning the shared grant the moment it is
    /// refreshed.
    #[tokio::test]
    async fn a_save_after_a_fallback_load_writes_the_refresh_back_to_the_shared_path() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.load().await.expect("load").expect("some");
        store.save(credentials("refreshed-cred")).await.expect("save");

        let primary = SecretPath::parse("github").expect("parse");
        assert!(
            backend.get(&primary).await.expect("get").is_none(),
            "a refresh of a fallback-origin credential must not create a machine-scoped copy"
        );

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        let raw = shared_after
            .get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
            .expect("v2 key");
        let decoded: StoredCredentials = serde_json::from_str(raw.expose_secret()).expect("decode");
        assert_eq!(decoded.client_id, "refreshed-cred");
    }

    #[tokio::test]
    async fn a_fallback_read_error_is_a_miss_not_a_failure() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());
        let read_from = store.read_from();

        fake(&backend).set_get_failure_at(shared, FakeFailure::Transport);

        let loaded = store
            .load()
            .await
            .expect("a fallback read error must not fail the load");
        assert!(loaded.is_none());
        assert_eq!(read_from.get(), None);
    }

    /// The hole this covers: a fallback payload that will not decode used to
    /// read as "nothing there", exactly like an empty fallback, so `load`
    /// would go on to report no stored credentials and open a fresh
    /// authorization flow against a path that in fact holds something this
    /// trg could not read. It has to surface the same way a corrupt primary
    /// payload does, naming the fallback path rather than the primary one.
    #[tokio::test]
    async fn a_corrupt_fallback_payload_is_reported_not_treated_as_absent() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(),
            SecretString::from("not json".to_string()),
        );
        backend.set(&shared, &map).await.expect("seed");

        let rendered = store
            .load()
            .await
            .expect_err("a fallback payload that will not decode must not read as absent")
            .to_string();
        assert!(
            rendered.contains("at `mcp/github`"),
            "must name the fallback path: {rendered}"
        );
        assert!(
            !rendered.contains("at `github`"),
            "must not name the primary path, which was never read: {rendered}"
        );
    }

    /// Same as the primary path: an unrecognized `credentials.vN` at the
    /// fallback is a newer `trg` sharing the backend, not a missing
    /// credential.
    #[tokio::test]
    async fn a_newer_version_fallback_payload_is_reported_not_treated_as_absent() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse("credentials.v7").unwrap(),
            SecretString::from("whatever a newer trg wrote".to_string()),
        );
        backend.set(&shared, &map).await.expect("seed");

        let rendered = store
            .load()
            .await
            .expect_err("an unknown newer version at the fallback should not decode as ours")
            .to_string();
        assert!(
            rendered.contains("at `mcp/github`"),
            "must name the fallback path: {rendered}"
        );
        assert!(
            !rendered.contains("at `github`"),
            "must not name the primary path, which was never read: {rendered}"
        );
        assert!(rendered.contains("credentials.v7"), "{rendered}");
        assert!(rendered.to_lowercase().contains("upgrade"), "{rendered}");
    }

    /// Case 2b: today's live shape, a `StoredCredentials` blob wrapped under
    /// the unversioned key. `load` has to migrate this silently, or every
    /// deployment that has ever authorized an MCP server would hit the same
    /// rewrite on its very next read.
    #[tokio::test]
    async fn load_migrates_the_unversioned_key_to_v2_and_keeps_siblings() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let json = serde_json::to_string(&credentials("abc")).expect("encode");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        seed.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(json.clone()),
        );
        backend.set(&path, &seed).await.expect("seed");

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "abc");

        let map = backend.get(&path).await.expect("get").expect("some");
        assert!(!map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));
        assert_eq!(
            map.get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
                .map(|v| v.expose_secret()),
            Some(json.as_str())
        );
        assert_eq!(
            map.get(&SecretKey::parse("api_key").unwrap())
                .map(|v| v.expose_secret()),
            Some("keep-me")
        );
    }

    /// Case 3: the pre-#73 shape, a bare `StoredCredentials` blob with no
    /// wrapping map at all. Real items like this still exist in the wild,
    /// with a populated `token_response` and a non-empty `granted_scopes`
    /// rather than the placeholder values a hand-written fixture would reach
    /// for; `issuer` is genuinely absent on one, since it predates that field.
    #[tokio::test]
    async fn load_migrates_a_raw_pre_versioning_legacy_payload_to_v2() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let legacy_raw = LEGACY_RAW_PAYLOAD.to_string();
        fake(&backend).set_get_failure(Some(FakeFailure::MalformedWithRaw(legacy_raw.clone())));

        let loaded = store.load().await.expect("load").expect("some");
        assert_eq!(loaded.client_id, "legacy-client");
        assert_eq!(
            loaded.token_response.expect("token_response").access_token().secret(),
            "fake-access-token"
        );

        fake(&backend).set_get_failure(None);
        let map = backend.get(&path).await.expect("get").expect("migrated");
        assert_eq!(
            map.get(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap())
                .map(|v| v.expose_secret()),
            Some(legacy_raw.as_str())
        );
        assert!(!map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));
    }

    #[tokio::test]
    async fn load_reports_an_unrecognized_newer_version_key_as_an_upgrade_prompt() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse("credentials.v7").unwrap(),
            SecretString::from("whatever a newer trg wrote".to_string()),
        );
        backend.set(&path, &map).await.expect("seed");

        let rendered = store
            .load()
            .await
            .expect_err("an unknown newer version should not decode as ours")
            .to_string();
        assert!(rendered.contains("credentials.v7"), "{rendered}");
        assert!(rendered.to_lowercase().contains("upgrade"), "{rendered}");
        assert!(!rendered.contains("trg mcp auth logout"), "{rendered}");
    }

    #[tokio::test]
    async fn load_reports_a_v2_value_that_is_not_stored_credentials_as_corrupt_without_serde_noise() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse(CREDENTIALS_KEY_V2).unwrap(),
            SecretString::from(r#"{"not":"credentials"}"#.to_string()),
        );
        backend.set(&path, &map).await.expect("seed");

        let rendered = store
            .load()
            .await
            .expect_err("an unparseable v2 value should not decode")
            .to_string();
        assert!(!rendered.contains("line"), "{rendered}");
        assert!(!rendered.contains("column"), "{rendered}");
        assert!(rendered.contains("trg mcp auth logout"), "{rendered}");
    }

    #[tokio::test]
    async fn load_reports_a_raw_payload_that_is_not_stored_credentials_as_corrupt() {
        let (backend, store) = store();
        fake(&backend).set_get_failure(Some(FakeFailure::MalformedWithRaw("not json at all".to_string())));

        let rendered = store
            .load()
            .await
            .expect_err("garbage should not parse as a legacy payload")
            .to_string();
        assert!(!rendered.contains("line"), "{rendered}");
        assert!(!rendered.contains("column"), "{rendered}");
        assert!(rendered.contains("trg mcp auth logout"), "{rendered}");
    }

    #[tokio::test]
    async fn load_returns_credentials_even_when_the_unversioned_migration_write_fails() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let json = serde_json::to_string(&credentials("abc")).expect("encode");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
        backend.set(&path, &seed).await.expect("seed");

        fake(&backend).set_set_failure(true);
        let loaded = store
            .load()
            .await
            .expect("a failed migration write should not fail the read")
            .expect("some");
        assert_eq!(loaded.client_id, "abc");
        fake(&backend).set_set_failure(false);
    }

    #[tokio::test]
    async fn load_returns_credentials_even_when_the_raw_legacy_migration_write_fails() {
        let (backend, store) = store();
        let legacy_raw = r#"{"client_id":"legacy","token_response":null,"granted_scopes":[]}"#.to_string();
        fake(&backend).set_get_failure(Some(FakeFailure::MalformedWithRaw(legacy_raw)));
        fake(&backend).set_set_failure(true);

        let loaded = store
            .load()
            .await
            .expect("a failed migration write should not fail the read")
            .expect("some");
        assert_eq!(loaded.client_id, "legacy");

        fake(&backend).set_get_failure(None);
        fake(&backend).set_set_failure(false);
    }

    #[tokio::test]
    async fn clear_removes_an_unversioned_legacy_key() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let json = serde_json::to_string(&credentials("abc")).expect("encode");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        seed.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
        backend.set(&path, &seed).await.expect("seed");

        store.clear().await.expect("clear");

        let map = backend.get(&path).await.expect("get").expect("siblings remain");
        assert!(!map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));
        assert!(map.contains_key(&SecretKey::parse("api_key").unwrap()));
    }

    #[tokio::test]
    async fn clear_deletes_the_whole_path_for_an_unreadable_raw_legacy_payload() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(
            SecretKey::parse("api_key").unwrap(),
            SecretString::from("would be lost"),
        );
        backend.set(&path, &seed).await.expect("seed");

        fake(&backend).set_get_failure(Some(FakeFailure::Malformed));
        store.clear().await.expect("clear an unreadable payload");
        fake(&backend).set_get_failure(None);

        assert!(backend.get(&path).await.expect("get").is_none());
    }
}

/// Recovery path for an item written before credentials moved into a keyed
/// map. `clear` must still be able to remove it, or the error message telling
/// the user to run `logout` then `login` would be a dead end.
#[cfg(all(test, target_os = "macos"))]
mod legacy_payload_tests {
    use super::*;
    use crate::secrets::KeychainBackend;

    fn test_path(ns: u32) -> SecretPath {
        SecretPath::parse(&format!("trg-test-legacy-{}-{ns}", std::process::id())).expect("parse")
    }

    fn seed_legacy(service: &str, path: &SecretPath) {
        let status = std::process::Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-A",
                "-s",
                service,
                "-a",
                path.as_str(),
                "-w",
                LEGACY_RAW_PAYLOAD,
            ])
            .status()
            .expect("spawn security");
        assert!(status.success(), "seed the legacy item");
    }

    #[tokio::test]
    #[ignore = "writes to the developer's real login keychain"]
    async fn load_migrates_a_legacy_payload_and_clear_still_removes_it() {
        let keychain = KeychainBackend::with_default_service();
        let path = test_path(1);
        seed_legacy(keychain.service(), &path);

        let backend = Backend::Keychain(keychain.clone());
        let store = OAuthCredentialStore::new(backend, path.clone(), path.as_str(), None);

        let loaded = store
            .load()
            .await
            .expect("a legacy payload should migrate silently")
            .expect("some");
        assert_eq!(loaded.client_id, "legacy-client");

        let map = keychain.get(&path).await.expect("get").expect("migrated");
        assert!(map.contains_key(&SecretKey::parse(CREDENTIALS_KEY_V2).unwrap()));
        assert!(!map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));

        store.clear().await.expect("clear");
        assert!(keychain.get(&path).await.expect("get").is_none());
    }
}
