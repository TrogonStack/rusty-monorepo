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

use crate::secrets::{Backend, SecretKey, SecretPath, SecretsError};

/// The key under which a server's OAuth credentials live.
pub const CREDENTIALS_KEY: &str = "credentials";

pub struct OAuthCredentialStore {
    backend: Backend,
    path: SecretPath,
}

impl OAuthCredentialStore {
    pub fn new(backend: Backend, path: SecretPath) -> Self {
        Self { backend, path }
    }

    fn key() -> SecretKey {
        SecretKey::parse(CREDENTIALS_KEY).expect("CREDENTIALS_KEY is a valid secret key")
    }
}

#[async_trait]
impl CredentialStore for OAuthCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(map) = self.backend.get(&self.path).await.map_err(to_auth_error)? else {
            return Ok(None);
        };
        let Some(raw) = map.get(&Self::key()) else {
            return Ok(None);
        };
        serde_json::from_str(raw.expose_secret()).map(Some).map_err(|e| {
            to_auth_error(SecretsError::Malformed {
                path: self.path.clone(),
                cause: e.to_string(),
            })
        })
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::InternalError(format!("encode credentials: {e}")))?;

        let mut map = self.backend.get(&self.path).await.unwrap_or(None).unwrap_or_default();
        map.insert(Self::key(), SecretString::from(json));

        self.backend.set(&self.path, &map).await.map_err(to_auth_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let Some(mut map) = self.backend.get(&self.path).await.unwrap_or(None) else {
            return self.backend.delete(&self.path).await.map_err(to_auth_error);
        };
        map.remove(&Self::key());
        if map.is_empty() {
            self.backend.delete(&self.path).await.map_err(to_auth_error)
        } else {
            self.backend.set(&self.path, &map).await.map_err(to_auth_error)
        }
    }
}

/// rmcp models every storage failure as `InternalError(String)`, so the
/// [`SecretsError`] variant survives only in the message.
fn to_auth_error(err: SecretsError) -> AuthError {
    match &err {
        SecretsError::Malformed { path, .. } => AuthError::InternalError(format!(
            "{err}. Run `trg mcp auth logout --server {path}` then `trg mcp auth login --server {path}` to re-authorize"
        )),
        _ => AuthError::InternalError(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{fake::FakeBackend, SecretMap};

    fn store() -> (Backend, OAuthCredentialStore) {
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("github").expect("parse");
        (backend.clone(), OAuthCredentialStore::new(backend, path))
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
    async fn load_reports_malformed_and_clear_still_removes_it() {
        let keychain = KeychainBackend::with_default_service();
        let path = test_path(1);
        seed_legacy(keychain.service(), &path);

        let backend = Backend::Keychain(keychain.clone());
        let store = OAuthCredentialStore::new(backend, path.clone());

        let err = store.load().await.expect_err("legacy payload should not decode");
        let rendered = err.to_string();
        assert!(rendered.contains("malformed payload"), "{rendered}");
        assert!(rendered.contains("trg mcp auth logout"), "{rendered}");

        store.clear().await.expect("clear");
        assert!(keychain.get(&path).await.expect("get").is_none());
    }
}
