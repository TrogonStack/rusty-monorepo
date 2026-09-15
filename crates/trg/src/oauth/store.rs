//! Adapter from rmcp's `CredentialStore` onto a [`Backend`].
//!
//! rmcp owns the `StoredCredentials` shape and takes ownership of the store, so
//! this is the one place left that needs `#[async_trait]`: the trait is rmcp's,
//! and it must stay dyn-compatible for them. Everything below it dispatches
//! statically.
//!
//! A whole `StoredCredentials` serializes into one key ([`CREDENTIALS_KEY`])
//! of the map stored at the server's [`SecretPath`], which keeps the backend
//! ignorant of OAuth and leaves room for other keys at the same path later.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use secrecy::{ExposeSecret, SecretString};

use crate::secrets::{Backend, SecretKey, SecretMap, SecretPath, SecretsError};
use crate::shell::quote_for_shell;

/// The key under which a server's OAuth credentials live in the map stored
/// at the server's [`SecretPath`].
pub const CREDENTIALS_KEY: &str = "credentials";

/// Everything `load`'s read can conclude once it has already tried
/// [`CREDENTIALS_KEY`].
///
/// Kept apart from [`SecretsError`], which the backend layer owns and which
/// therefore stays ignorant of what an OAuth credential looks like.
#[derive(Debug, thiserror::Error)]
enum CredentialReadError {
    /// The stored payload failed to parse. A `serde_json` decode error
    /// would say this too, but with a line and column a person cannot act
    /// on; this names the actual situation and its fix instead.
    #[error(
        "no OAuth credentials could be read at `{path}`: the stored payload does not match any \
         shape this trg understands"
    )]
    Corrupt { path: SecretPath },
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
    /// Set the first time `clear` invalidates a credential this store itself
    /// read from the fallback. `clear` never deletes the shared path, so
    /// without this a `load` right afterward would serve that same
    /// credential straight back; once set, `load` stops trying the fallback
    /// for the rest of this store's life. Private to this store, unlike
    /// [`StorageFailure`] and [`FallbackRead`]: nothing outside needs to
    /// observe it.
    fallback_disabled: AtomicBool,
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
            fallback_disabled: AtomicBool::new(false),
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
    /// instead of reporting a clean logout it cannot vouch for.
    ///
    /// What counts as reachable is an OAuth credential, not a populated
    /// path. [`Self::load_from_fallback`] already reads it that way, and a
    /// path holding only unrelated sibling keys would otherwise have
    /// `logout` tell the operator this machine keeps authenticating from a
    /// shared credential that is not there.
    ///
    /// A payload that is present but will not decode still counts as
    /// reachable rather than clean, and deliberately does not propagate the
    /// decoder error: `logout` calls this only after `clear` has already
    /// deleted the machine-scoped path, so failing here would leave the
    /// operator with a half-done logout and no answer. Erring toward telling
    /// them something is there is the safe direction.
    pub async fn fallback_is_reachable(&self) -> Result<Option<SecretPath>, AuthError> {
        let Some(fallback) = self.fallback.clone() else {
            return Ok(None);
        };
        match self.backend.get(&fallback).await {
            Ok(Some(map)) => match self.decode_from_fallback_map(&fallback, &map) {
                Ok(Some(_)) | Err(_) => Ok(Some(fallback)),
                Ok(None) => Ok(None),
            },
            Ok(None) => Ok(None),
            Err(e) => Err(self.to_auth_error(e)),
        }
    }

    fn key() -> SecretKey {
        SecretKey::parse(CREDENTIALS_KEY).expect("CREDENTIALS_KEY is a valid secret key")
    }
}

#[async_trait]
impl CredentialStore for OAuthCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // `read_from` is deliberately not reset on the way in. Resetting up
        // front would clear it for reads that then fail, and a read that
        // failed says nothing about where the credential rmcp is still
        // holding came from; the next token-carrying `save` would take the
        // empty slot at face value and copy a fallback-origin grant onto the
        // machine-scoped path, the replay `machine_id` exists to prevent,
        // reached through an error path. Only an outcome this can vouch for
        // moves the slot, so every arm below either records or deliberately
        // leaves it alone. A store reused across more than one `load` is
        // still safe: each vouchable outcome overwrites the last one.
        match self.backend.get(&self.path).await {
            // Once `clear` has invalidated a fallback-origin credential, this
            // store must not turn around and read it straight back.
            Ok(None) if self.fallback_disabled.load(Ordering::Relaxed) => {
                self.read_from.record(None);
                Ok(None)
            }
            Ok(None) => self.load_from_fallback().await,
            // The primary path answered, so whatever comes of decoding it,
            // nothing is being served from the fallback any more.
            Ok(Some(map)) => {
                self.read_from.record(None);
                self.load_from_map(map).await
            }
            // The outer map itself would not decode. The primary path holds
            // a payload either way, so it cannot be reported as absent.
            Err(SecretsError::Malformed { .. }) => {
                self.read_from.record(None);
                Err(self.credential_read_error(CredentialReadError::Corrupt {
                    path: self.path.clone(),
                }))
            }
            // Transient: leaves `read_from` untouched on purpose.
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
    ///
    /// That write-back-to-origin rule is only for a refresh, which always
    /// carries tokens. rmcp also calls `save` to discard a grant, stripped
    /// of tokens, when it notices the authorization server's issuer changed
    /// but the client ID is portable (CIMD) and worth keeping; that is not a
    /// rotation, it is one machine invalidating a credential, and a
    /// fallback-origin credential is not this machine's to invalidate. A
    /// save with no tokens always targets `self.path`, never the fallback.
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::InternalError(format!("encode credentials: {e}")))?;
        let target = if credentials.token_response.is_some() {
            self.read_from.get().unwrap_or_else(|| self.path.clone())
        } else {
            self.path.clone()
        };

        let mut map = match self.backend.get(&target).await {
            Ok(existing) => existing.unwrap_or_default(),
            // An outer payload that will not decode leaves nothing structured
            // behind it: this trg cannot parse a map from it, so there are no
            // sibling keys to lose by starting fresh.
            Err(SecretsError::Malformed { .. }) => SecretMap::new(),
            // Anything else is transient. Writing through it would replace the
            // whole map with just this key and take any siblings with it.
            Err(e) => return Err(self.to_auth_error(e)),
        };
        map.insert(Self::key(), SecretString::from(json));

        self.backend.set(&target, &map).await.map_err(|e| self.to_auth_error(e))
    }

    /// Always deletes `self.path`, never the fallback: deleting the shared
    /// path would sign out every other machine still reading from it. A
    /// caller that needs to know whether this machine is still effectively
    /// signed in afterward (because the fallback still holds a credential)
    /// has to check with a subsequent `load`; `clear` itself does not report
    /// it.
    ///
    /// If the credential this invalidates was itself read from the fallback
    /// (`self.read_from` records that), the shared path survives untouched,
    /// so the caller's belief that it discarded a credential would otherwise
    /// be wrong: the very next `load` on this store would serve the same
    /// fallback credential straight back. `self.fallback_disabled` closes
    /// that gap by stopping this store, for the rest of its life, from
    /// reading the fallback at all; a `tracing::warn!` names what is still
    /// out there and how to actually remove it, since this store has no way
    /// to remove it itself.
    async fn clear(&self) -> Result<(), AuthError> {
        let existing = match self.backend.get(&self.path).await {
            Ok(existing) => existing,
            // `logout` is the documented recovery for an unreadable payload, so
            // it has to drop the path rather than refuse.
            Err(SecretsError::Malformed { .. }) => None,
            Err(e) => return Err(self.to_auth_error(e)),
        };
        let result = if let Some(mut map) = existing {
            map.remove(&Self::key());
            if map.is_empty() {
                self.backend.delete(&self.path).await.map_err(|e| self.to_auth_error(e))
            } else {
                self.backend
                    .set(&self.path, &map)
                    .await
                    .map_err(|e| self.to_auth_error(e))
            }
        } else {
            self.backend.delete(&self.path).await.map_err(|e| self.to_auth_error(e))
        };

        if let Some(shared) = self.read_from.get() {
            self.fallback_disabled.store(true, Ordering::Relaxed);
            let removal = match self.backend.removal_command(&shared) {
                Some(command) => format!("remove it with `{command}`"),
                None => "remove it directly against the backend".to_string(),
            };
            tracing::warn!(
                server = %self.server,
                path = %shared,
                "credentials for `{}` are still sitting at the shared path `{shared}`, bound to the \
                 authorization server they were just invalidated against; every machine reading that \
                 path is affected; {removal} to sign out every machine using it",
                self.server
            );
        }

        result
    }
}

impl OAuthCredentialStore {
    /// `path` holds nothing. Before answering `None`, check whether the
    /// credential is still sitting at the pre-`machine_id` shared path: it is
    /// safe to read from there, but never to copy, move, or delete, so a hit
    /// here is reported rather than acted on.
    async fn load_from_fallback(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(fallback) = self.fallback.clone() else {
            self.read_from.record(None);
            return Ok(None);
        };

        let map = match self.backend.get(&fallback).await {
            Ok(Some(map)) => map,
            Ok(None) => {
                self.read_from.record(None);
                return Ok(None);
            }
            // A read error at the fallback is not a reason to refuse a login
            // that would otherwise proceed normally against an empty primary
            // path: the fallback is a courtesy, not a dependency. It is a
            // reason to leave `read_from` alone, though: answering `Ok(None)`
            // here is this method admitting it could not tell, and clearing
            // the slot on the way out would turn that into a claim that
            // nothing was ever read from the fallback. See `load`.
            Err(e) => {
                tracing::debug!(
                    path = %fallback,
                    error = %e,
                    "could not read the shared OAuth credential fallback path"
                );
                return Ok(None);
            }
        };

        // Same distinction once more: a fallback that decodes to no credential
        // of ours is a clean empty answer, one that will not decode at all is
        // not, and only the first may clear the slot.
        let credentials = match self.decode_from_fallback_map(&fallback, &map) {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                self.read_from.record(None);
                return Ok(None);
            }
            Err(e) => return Err(e),
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
    /// writing anything back: the fallback path stays exactly as found.
    ///
    /// Applies the same corruption handling [`Self::load_from_map`] applies
    /// to the primary path: a fallback that holds something this trg cannot
    /// read is not the same as a fallback that holds nothing, and reporting
    /// it as absent would send `load` on to open a fresh authorization flow
    /// against a path that in fact has a credential on it. A genuinely empty
    /// fallback still answers `Ok(None)`.
    fn decode_from_fallback_map(
        &self,
        path: &SecretPath,
        map: &SecretMap,
    ) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(value) = map.get(&Self::key()) else {
            return Ok(None);
        };
        self.decode_or_corrupt_at(path, value.expose_secret()).map(Some)
    }

    /// The outer map decoded. Reads [`CREDENTIALS_KEY`] if present; otherwise
    /// the path simply holds none of our keys.
    async fn load_from_map(&self, map: SecretMap) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(value) = map.get(&Self::key()) else {
            return Ok(None);
        };
        let raw = value.expose_secret().to_string();
        self.decode_or_corrupt(&raw).map(Some)
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
        let message = format!("{err}. {}", self.recovery_advice());
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

#[cfg(test)]
mod tests {
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

    /// Like [`credentials`], but with a real `token_response`, the way an
    /// actual token refresh always looks. `credentials` deliberately leaves
    /// it `None`, which after the fix in this module means a call built from
    /// it always targets the primary path; a test standing in for a refresh
    /// needs this one instead.
    fn credentials_with_token(client_id: &str) -> StoredCredentials {
        let raw = format!(
            r#"{{"client_id":"{client_id}","token_response":{{"access_token":"fake-access-token","token_type":"bearer","expires_in":3600,"refresh_token":"fake-refresh-token","scope":"read write"}},"granted_scopes":["scope"],"token_received_at":42}}"#
        );
        serde_json::from_str(&raw).expect("decode")
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
        assert!(map.contains_key(&SecretKey::parse(CREDENTIALS_KEY).unwrap()));
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

    /// The hole this covers: `clear` only ever deletes `self.path`, which on
    /// a fallback-origin load is already empty, so a `clear` that is meant to
    /// invalidate a fallback-origin credential (rmcp does exactly this on an
    /// issuer change it does not treat as CIMD-portable) used to be a no-op
    /// the caller believed had worked, and the very next `load` on the same
    /// store served the same shared credential straight back. Once `clear`
    /// has run against a fallback-origin credential, this store must stop
    /// reading the fallback at all.
    #[tokio::test]
    async fn load_after_clear_on_a_fallback_origin_store_never_serves_the_shared_credential_again() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json.clone()),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.load().await.expect("load").expect("some");
        store.clear().await.expect("clear");

        assert!(
            store.load().await.expect("load").is_none(),
            "a load right after clear must not serve the credential clear just invalidated"
        );
        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        assert_eq!(
            shared_after
                .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
                .map(|v| v.expose_secret()),
            Some(shared_json.as_str()),
            "clear must never delete the shared path itself, only stop this store from reading it"
        );
    }

    /// A `clear` with nothing to invalidate (no prior `load`, or a `load`
    /// that hit the primary path) has no fallback-origin credential to worry
    /// about, so it must not disable the fallback for a later load that
    /// legitimately wants it.
    #[tokio::test]
    async fn clear_with_no_prior_fallback_load_leaves_the_fallback_usable() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.clear().await.expect("clear with nothing loaded yet");

        assert!(
            store.load().await.expect("load").is_some(),
            "a clear that never touched a fallback-origin credential must not disable the fallback"
        );
    }

    #[tokio::test]
    async fn load_reads_the_credentials_key_directly() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let json = serde_json::to_string(&credentials("direct")).expect("encode");
        let mut map = SecretMap::new();
        map.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
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
        map.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
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
        map.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
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
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
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
            .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
            .expect("credentials key");
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
                .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
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
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json.clone()),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.save(credentials("primary-cred")).await.expect("save");

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        assert_eq!(
            shared_after
                .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
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
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.load().await.expect("load").expect("some");
        store
            .save(credentials_with_token("refreshed-cred"))
            .await
            .expect("save");

        let primary = SecretPath::parse("github").expect("parse");
        assert!(
            backend.get(&primary).await.expect("get").is_none(),
            "a refresh of a fallback-origin credential must not create a machine-scoped copy"
        );

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        let raw = shared_after
            .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
            .expect("credentials key");
        let decoded: StoredCredentials = serde_json::from_str(raw.expose_secret()).expect("decode");
        assert_eq!(decoded.client_id, "refreshed-cred");
    }

    /// The hole this covers: rmcp also calls `save` to discard a grant,
    /// tokens stripped, when it notices an issuer change but keeps a
    /// portable CIMD client ID. Before the fix, `save`'s write-back-to-origin
    /// rule sent that token-less record to the shared path exactly like a
    /// refresh would, wiping the tokens every other machine reading that
    /// path was using. A save with nothing left to refresh is never this
    /// machine's call to make on a credential it does not own, so it must
    /// land on the primary path and leave the shared payload untouched.
    #[tokio::test]
    async fn a_token_less_save_after_a_fallback_load_never_touches_the_shared_path() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json.clone()),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        store.load().await.expect("load").expect("some");
        store.save(credentials("discarded-cred")).await.expect("save");

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        assert_eq!(
            shared_after
                .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
                .map(|v| v.expose_secret()),
            Some(shared_json.as_str()),
            "a token-less save must never touch a fallback-origin shared payload"
        );

        let primary = SecretPath::parse("github").expect("parse");
        let primary_after = backend.get(&primary).await.expect("get").expect("some");
        let raw = primary_after
            .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
            .expect("credentials key");
        let decoded: StoredCredentials = serde_json::from_str(raw.expose_secret()).expect("decode");
        assert_eq!(
            decoded.client_id, "discarded-cred",
            "a token-less save must still land somewhere: the primary path, not the shared one"
        );
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
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
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

    #[tokio::test]
    async fn load_reports_a_value_that_is_not_stored_credentials_as_corrupt_without_serde_noise() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut map = SecretMap::new();
        map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(r#"{"not":"credentials"}"#.to_string()),
        );
        backend.set(&path, &map).await.expect("seed");

        let rendered = store
            .load()
            .await
            .expect_err("an unparseable value should not decode")
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
            .expect_err("garbage should not parse")
            .to_string();
        assert!(!rendered.contains("line"), "{rendered}");
        assert!(!rendered.contains("column"), "{rendered}");
        assert!(rendered.contains("trg mcp auth logout"), "{rendered}");
    }

    #[tokio::test]
    async fn clear_deletes_the_whole_path_for_an_unreadable_payload() {
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

    /// The hole this covers: `load` used to clear `read_from` on the way in,
    /// before it knew how the read would turn out, and a fallback read error
    /// is reported to rmcp as `Ok(None)` so as not to block a login. Together
    /// those turned "we could not tell" into "nothing was read from the
    /// fallback", and the next refresh wrote a fallback-origin grant onto the
    /// machine-scoped path, leaving two paths refreshing one grant: the
    /// replay `machine_id` exists to prevent, reached through an error path.
    #[tokio::test]
    async fn a_failed_fallback_read_does_not_retarget_the_next_refresh_at_the_primary() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let primary = SecretPath::parse("github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        let read_from = store.read_from();
        store.load().await.expect("load").expect("some");
        assert_eq!(
            read_from.get().as_ref(),
            Some(&shared),
            "the first load reads the shared path"
        );

        fake(&backend).set_get_failure_at(shared.clone(), FakeFailure::Transport);
        assert!(
            store
                .load()
                .await
                .expect("a fallback read error must not fail the load")
                .is_none(),
            "a fallback the store could not read still answers as absent, so a login is not blocked"
        );
        assert_eq!(
            read_from.get().as_ref(),
            Some(&shared),
            "a read that failed says nothing about where rmcp's credential came from, so it must not clear the slot"
        );
        fake(&backend).clear_get_failure_at(&shared);

        store
            .save(credentials_with_token("refreshed-cred"))
            .await
            .expect("save");

        let shared_after = backend.get(&shared).await.expect("get").expect("still there");
        let raw = shared_after
            .get(&SecretKey::parse(CREDENTIALS_KEY).unwrap())
            .expect("credentials key");
        let decoded: StoredCredentials = serde_json::from_str(raw.expose_secret()).expect("decode");
        assert_eq!(
            decoded.client_id, "refreshed-cred",
            "the refresh must still write back to the path it was read from"
        );
        assert!(
            backend.get(&primary).await.expect("get").is_none(),
            "the refresh must not have been copied onto the machine-scoped path"
        );
    }

    /// The counterpart: a fallback that answers cleanly with no credential of
    /// ours *is* something the store can vouch for, so it must clear the slot
    /// rather than leave a stale origin behind for the next save.
    #[tokio::test]
    async fn a_clean_empty_fallback_read_does_clear_the_recorded_origin() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        let read_from = store.read_from();
        store.load().await.expect("load").expect("some");
        assert_eq!(read_from.get().as_ref(), Some(&shared));

        backend
            .delete(&shared)
            .await
            .expect("someone removed the shared credential");
        assert!(store.load().await.expect("load").is_none());
        assert_eq!(
            read_from.get(),
            None,
            "a fallback that is cleanly empty means nothing is being served from it any more"
        );
    }

    /// The hole this covers: `fallback_is_reachable` answered on whether the
    /// path held a map at all, so a shared path holding only unrelated
    /// sibling keys made `logout` tell the operator this machine would keep
    /// authenticating from a shared credential that was never there. `load`
    /// already reads that same map as holding nothing of ours.
    #[tokio::test]
    async fn fallback_is_reachable_ignores_a_path_that_only_holds_sibling_keys() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let mut sibling_only = SecretMap::new();
        sibling_only.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&shared, &sibling_only).await.expect("seed the shared path");

        assert_eq!(
            store.fallback_is_reachable().await.expect("reachable"),
            None,
            "sibling keys are not an OAuth credential, and logout must not claim they are"
        );
    }

    #[tokio::test]
    async fn fallback_is_reachable_reports_a_recognized_credential() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let shared_json = serde_json::to_string(&credentials_with_token("shared-cred")).expect("encode");
        let mut shared_map = SecretMap::new();
        shared_map.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from(shared_json),
        );
        backend.set(&shared, &shared_map).await.expect("seed the shared path");

        assert_eq!(store.fallback_is_reachable().await.expect("reachable"), Some(shared),);
    }

    /// A payload that will not decode is still a payload sitting there, and
    /// `logout` has already deleted the machine-scoped path by the time it
    /// asks, so failing here would leave a half-done logout with no answer.
    #[tokio::test]
    async fn fallback_is_reachable_reports_a_corrupt_shared_payload_as_still_there() {
        let shared = SecretPath::parse("mcp/github").expect("parse");
        let (backend, store) = store_with_fallback(shared.clone());

        let mut corrupt = SecretMap::new();
        corrupt.insert(
            SecretKey::parse(CREDENTIALS_KEY).unwrap(),
            SecretString::from("{not json"),
        );
        backend.set(&shared, &corrupt).await.expect("seed the shared path");

        assert_eq!(
            store.fallback_is_reachable().await.expect("reachable"),
            Some(shared),
            "erring toward telling the operator something is there is the safe direction"
        );
    }
}
