//! Adapter from rmcp's `CredentialStore` onto a [`Backend`].
//!
//! rmcp owns the `StoredCredentials` shape and takes ownership of the store, so
//! this is the one place left that needs `#[async_trait]`: the trait is rmcp's,
//! and it must stay dyn-compatible for them. Everything below it dispatches
//! statically.
//!
//! A whole `StoredCredentials` serializes into one key ([`CREDENTIALS_KEY`]) of
//! the map stored at the server's [`SecretPath`], which keeps the backend
//! ignorant of OAuth and leaves room for other keys at the same path later.

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use secrecy::{ExposeSecret, SecretString};

use crate::oauth::flow::quote_for_shell;
use crate::secrets::{Backend, SecretKey, SecretMap, SecretPath, SecretsError};

/// The key under which a server's OAuth credentials live.
pub const CREDENTIALS_KEY: &str = "credentials";

pub struct OAuthCredentialStore {
    backend: Backend,
    path: SecretPath,
    /// The `--server` name recovery advice has to quote. Not recoverable from
    /// `path`: only the Keychain stores a server under its bare name, and
    /// OpenBao stores it under `mcp/<machine_id>/<server>`.
    server: String,
}

impl OAuthCredentialStore {
    pub fn new(backend: Backend, path: SecretPath, server: impl Into<String>) -> Self {
        Self {
            backend,
            path,
            server: server.into(),
        }
    }

    fn key() -> SecretKey {
        SecretKey::parse(CREDENTIALS_KEY).expect("CREDENTIALS_KEY is a valid secret key")
    }
}

#[async_trait]
impl CredentialStore for OAuthCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(map) = self.backend.get(&self.path).await.map_err(|e| self.to_auth_error(e))? else {
            return Ok(None);
        };
        let Some(raw) = map.get(&Self::key()) else {
            return Ok(None);
        };
        serde_json::from_str(raw.expose_secret()).map(Some).map_err(|e| {
            self.to_auth_error(SecretsError::Malformed {
                path: self.path.clone(),
                cause: e.to_string(),
            })
        })
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::InternalError(format!("encode credentials: {e}")))?;

        let mut map = match self.backend.get(&self.path).await {
            Ok(existing) => existing.unwrap_or_default(),
            // An unreadable payload is the one read failure worth overwriting:
            // no sibling keys survived decoding, so none can be lost.
            Err(SecretsError::Malformed { .. }) => SecretMap::new(),
            // Anything else is transient. Writing through it would replace the
            // whole map with just this key and take any siblings with it.
            Err(e) => return Err(self.to_auth_error(e)),
        };
        map.insert(Self::key(), SecretString::from(json));

        self.backend
            .set(&self.path, &map)
            .await
            .map_err(|e| self.to_auth_error(e))
    }

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
    /// rmcp models every storage failure as `InternalError(String)`, so the
    /// [`SecretsError`] variant survives only in the message.
    fn to_auth_error(&self, err: SecretsError) -> AuthError {
        match &err {
            SecretsError::Malformed { .. } => {
                let server = quote_for_shell(&self.server);
                AuthError::InternalError(format!(
                    "{err}. Run `trg mcp auth logout --server {server}` then `trg mcp auth login --server {server}` to re-authorize"
                ))
            }
            _ => AuthError::InternalError(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{fake::FakeBackend, FakeFailure};

    fn store() -> (Backend, OAuthCredentialStore) {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("github").expect("parse");
        (backend.clone(), OAuthCredentialStore::new(backend, path, "github"))
    }

    fn credentials(client_id: &str) -> StoredCredentials {
        StoredCredentials::new(client_id.to_string(), None, vec!["scope".to_string()], Some(42))
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

    #[tokio::test]
    async fn save_propagates_a_read_failure_rather_than_clobbering_siblings() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");

        let Backend::Fake(fake) = &backend else {
            unreachable!("store() builds a fake")
        };
        fake.set_get_failure(Some(FakeFailure::Transport));
        store
            .save(credentials("abc"))
            .await
            .expect_err("should not write through a read failure");
        fake.set_get_failure(None);

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
        let Backend::Fake(fake) = &backend else {
            unreachable!("store() builds a fake")
        };
        fake.set_get_failure(Some(FakeFailure::Malformed));
        store
            .save(credentials("abc"))
            .await
            .expect("save over an unreadable payload");
        fake.set_get_failure(None);

        assert_eq!(store.load().await.expect("load").expect("some").client_id, "abc");
    }

    #[tokio::test]
    async fn clear_propagates_a_read_failure_rather_than_deleting_the_path() {
        let (backend, store) = store();
        let path = SecretPath::parse("github").expect("parse");
        let mut seed = SecretMap::new();
        seed.insert(SecretKey::parse("api_key").unwrap(), SecretString::from("keep-me"));
        backend.set(&path, &seed).await.expect("seed");

        let Backend::Fake(fake) = &backend else {
            unreachable!("store() builds a fake")
        };
        fake.set_get_failure(Some(FakeFailure::Transport));
        store
            .clear()
            .await
            .expect_err("should not delete through a read failure");
        fake.set_get_failure(None);

        assert!(backend.get(&path).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn malformed_recovery_command_is_copy_pasteable_for_any_server_name() {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("my server").expect("parse");
        let store = OAuthCredentialStore::new(backend.clone(), path, "my server");

        let Backend::Fake(fake) = &backend else { unreachable!() };
        fake.set_get_failure(Some(FakeFailure::Malformed));

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
        let store = OAuthCredentialStore::new(backend.clone(), path, "github");

        let Backend::Fake(fake) = &backend else { unreachable!() };
        fake.set_get_failure(Some(FakeFailure::Malformed));

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
                r#"{"client_id":"legacy","granted_scopes":[]}"#,
            ])
            .status()
            .expect("spawn security");
        assert!(status.success(), "seed the legacy item");
    }

    #[tokio::test]
    #[ignore = "writes to the developer's real login keychain"]
    async fn load_reports_malformed_and_clear_still_removes_it() {
        let keychain = KeychainBackend::with_default_service();
        let path = test_path(1);
        seed_legacy(keychain.service(), &path);

        let backend = Backend::Keychain(keychain.clone());
        let store = OAuthCredentialStore::new(backend, path.clone(), path.as_str());

        let err = store.load().await.expect_err("legacy payload should not decode");
        let rendered = err.to_string();
        assert!(rendered.contains("malformed payload"), "{rendered}");
        assert!(rendered.contains("trg mcp auth logout"), "{rendered}");

        store.clear().await.expect("clear");
        assert!(keychain.get(&path).await.expect("get").is_none());
    }
}
