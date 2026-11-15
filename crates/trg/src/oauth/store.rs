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

mod constants;
pub use constants::CREDENTIALS_KEY;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use secrecy::{ExposeSecret, SecretString};

use crate::oauth::telemetry::CredentialAccess;
use crate::secrets::{Backend, SecretKey, SecretMap, SecretPath, SecretsError};
use crate::shell::quote_for_shell;

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
        CredentialAccess::CheckShared
            .traced(&self.server, self.shared_is_reachable())
            .await
    }

    async fn shared_is_reachable(&self) -> Result<Option<SecretPath>, AuthError> {
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
        CredentialAccess::Load.traced(&self.server, self.read()).await
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        CredentialAccess::Save
            .traced(&self.server, self.write(credentials))
            .await
    }

    async fn clear(&self) -> Result<(), AuthError> {
        CredentialAccess::Clear.traced(&self.server, self.erase()).await
    }
}

impl OAuthCredentialStore {
    async fn read(&self) -> Result<Option<StoredCredentials>, AuthError> {
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
    async fn write(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
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
    async fn erase(&self) -> Result<(), AuthError> {
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
                removal,
                "credentials remain at the shared path bound to the invalidated authorization server; \
                 every machine reading that path is affected"
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
            "read OAuth credentials from the pre-machine_id shared path; they remain readable by \
             other machines sharing it; run trg mcp auth login for this server to write machine-scoped credentials"
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
#[path = "store_tests.rs"]
mod tests;
