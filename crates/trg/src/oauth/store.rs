//! Persistent credential storage backing rmcp's `CredentialStore`.
//!
//! macOS Keychain only for this milestone (see `crates/trg/PLAN.md`). No file
//! fallback by design: if Keychain is unavailable we surface the error rather
//! than spill OAuth tokens onto disk under `$HOME`.
//!
//! # Why we shell out to `/usr/bin/security` instead of using the `keyring` crate
//!
//! macOS's legacy file-based Keychain pins each item's ACL to the calling
//! process's **codesign identity** (its `cdhash`). `cargo build` produces a
//! fresh ad-hoc signature on every rebuild, so a binary built five minutes ago
//! looks like a *different* application to Keychain Services. The next read
//! pops a blocking GUI dialog ("X wants to use your `login` keychain") —
//! interactive use can click through, but `trg mcp proxy` is spawned by Cursor
//! as a headless child with no foreground window, so the prompt queues
//! invisibly and the child hangs (manifesting as MCP error -32000).
//!
//! The Apple C API that fixes this is `SecKeychainItemCreateFromContent` with
//! a `SecAccess` whose trusted-applications list is empty (= "any app may
//! access, no codesign check"). That's what `/usr/bin/security
//! add-generic-password -A` does internally, and it's the posture every other
//! macOS dev-tool CLI takes for the same reason (`1password-cli`, `gh`, etc.).
//!
//! The Rust `keyring` crate does **not** expose this knob, and its maintainer
//! has explicitly declared it out of scope for the legacy file-based Keychain.
//! The canonical upstream issues:
//!
//! - <https://github.com/open-source-cooperative/keyring-rs/issues/272> —
//!   "Tauri with keyring - asking for permission?" Same root cause; maintainer
//!   confirms the ACL settings "aren't available via `keyring`" and recommends
//!   switching to the data-protection keychain (see next paragraph).
//! - <https://github.com/open-source-cooperative/keyring-rs/issues/23> —
//!   "macOS requests password twice for each `get_password`". Earliest report
//!   of the same symptom; closed with "instruct users to Always Allow".
//!
//! Their recommended workaround — switch to the data-protection (modern)
//! Keychain via `apple-native-keyring-store`'s `protected` feature — requires
//! a provisioning profile and is documented as unusable for command-line
//! tools in that crate's own module docs:
//! <https://docs.rs/apple-native-keyring-store/1.0.0/apple_native_keyring_store/protected/index.html>
//! (verbatim: *"Since command-line tools cannot be code-signed, there's not
//! much point in their using this module."*).
//!
//! `security-framework` likewise wraps only `SecKeychainAddGenericPassword`,
//! which doesn't take a `SecAccess` parameter — see
//! `os::macos::passwords.rs::add_generic_password` (passes `ptr::null_mut()`).
//! Recent access-control work in that crate targets the data-protection
//! keychain path only, not the legacy ACL we need:
//!
//! - <https://github.com/kornelski/rust-security-framework/pull/178> —
//!   "Support access control options" (data-protection keychain).
//! - <https://github.com/kornelski/rust-security-framework/pull/220> —
//!   "Support user-defined generic password options/attributes"
//!   (data-protection keychain).
//!
//! # What this means in practice
//!
//! - `add-generic-password -U -A`: creates the item with an empty
//!   trusted-applications ACL, so subsequent reads never trigger a prompt
//!   regardless of which binary cdhash made the call.
//! - The secret is passed with `-w` on the child process argv (macOS `security`
//!   does not accept this payload from stdin for `add-generic-password`). The
//!   process is short-lived; avoiding argv would require a native Keychain API
//!   path that still reproduces the `-A` ACL semantics above.
//! - `find-generic-password -w`: returns the secret to stdout; we parse the
//!   "could not be found" stderr token to distinguish "no such item" from a
//!   genuine error.
//! - `delete-generic-password`: idempotent on missing items via the same
//!   stderr check.
//!
//! Cost is one process spawn per call (~3-5 ms on Apple Silicon). For the
//! `trg mcp proxy` workload (one load at startup, one save per token refresh)
//! this is unmeasurable. The benefit is that we avoid pulling in the
//! `keyring` / `keyring-core` / `apple-native-keyring-store` /
//! `security-framework` / `core-foundation` dependency chain while remaining
//! exactly as auditable.

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use tokio::process::Command;

pub const KEYRING_SERVICE: &str = "trg MCP Credentials";

const SECURITY_BIN: &str = "/usr/bin/security";

pub struct KeychainCredentialStore {
    server_name: String,
}

impl KeychainCredentialStore {
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
        }
    }
}

fn err(msg: impl Into<String>) -> AuthError {
    AuthError::InternalError(msg.into())
}

fn unsupported_platform() -> AuthError {
    AuthError::InternalError(
        "OAuth credential storage is supported only on macOS in this milestone".to_string(),
    )
}

#[async_trait]
impl CredentialStore for KeychainCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        if !cfg!(target_os = "macos") {
            return Err(unsupported_platform());
        }
        let out = Command::new(SECURITY_BIN)
            .args([
                "find-generic-password",
                "-s",
                KEYRING_SERVICE,
                "-a",
                &self.server_name,
                "-w",
            ])
            .output()
            .await
            .map_err(|e| err(format!("security find-generic-password: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("could not be found") {
                return Ok(None);
            }
            return Err(err(format!("security find-generic-password failed: {}", stderr.trim())));
        }

        let secret = String::from_utf8_lossy(&out.stdout);
        let secret = secret.trim_end_matches('\n');
        let credentials: StoredCredentials =
            serde_json::from_str(secret).map_err(|e| err(format!("decode keychain payload: {e}")))?;
        Ok(Some(credentials))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if !cfg!(target_os = "macos") {
            return Err(unsupported_platform());
        }
        let json = serde_json::to_string(&credentials).map_err(|e| err(e.to_string()))?;

        let out = Command::new(SECURITY_BIN)
            .args([
                "add-generic-password",
                "-U", // update in place if entry exists
                "-A", // allow access from any application (no codesign-pinned ACL)
                "-s",
                KEYRING_SERVICE,
                "-a",
                &self.server_name,
                "-w",
                &json,
            ])
            .output()
            .await
            .map_err(|e| err(format!("security add-generic-password: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(err(format!("security add-generic-password failed: {}", stderr.trim())));
        }
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        if !cfg!(target_os = "macos") {
            return Err(unsupported_platform());
        }
        let out = Command::new(SECURITY_BIN)
            .args([
                "delete-generic-password",
                "-s",
                KEYRING_SERVICE,
                "-a",
                &self.server_name,
            ])
            .output()
            .await
            .map_err(|e| err(format!("security delete-generic-password: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("could not be found") {
                return Ok(());
            }
            return Err(err(format!(
                "security delete-generic-password failed: {}",
                stderr.trim()
            )));
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    mod macos_keychain {
        use super::*;

        fn test_account(ns: u32) -> String {
            format!("trg-test-{}-{ns}", std::process::id())
        }

        #[tokio::test]
        async fn load_fresh_returns_none() {
            let account = test_account(1);
            let store = KeychainCredentialStore::new(&account);
            let loaded = store.load().await.expect("load");
            assert!(loaded.is_none());
            let _ = store.clear().await;
        }

        #[tokio::test]
        async fn save_load_roundtrip() {
            let account = test_account(2);
            let store = KeychainCredentialStore::new(&account);
            let credentials = StoredCredentials::new(
                "client-abc".to_string(),
                None,
                vec!["scope1".to_string(), "scope2".to_string()],
                Some(42),
            );

            store.save(credentials.clone()).await.expect("save");
            let loaded = store.load().await.expect("load").expect("some");
            assert_eq!(loaded.client_id, credentials.client_id);
            assert_eq!(loaded.granted_scopes, credentials.granted_scopes);

            store.clear().await.expect("clear");
        }

        #[tokio::test]
        async fn save_twice_overwrites() {
            let account = test_account(3);
            let store = KeychainCredentialStore::new(&account);

            let first = StoredCredentials::new("first".to_string(), None, vec![], None);
            let second = StoredCredentials::new("second".to_string(), None, vec!["s".to_string()], Some(99));

            store.save(first).await.expect("save first");
            store.save(second.clone()).await.expect("save second");
            let loaded = store.load().await.expect("load").expect("some");
            assert_eq!(loaded.client_id, second.client_id);
            assert_eq!(loaded.granted_scopes, second.granted_scopes);

            store.clear().await.expect("clear");
        }

        #[tokio::test]
        async fn clear_after_save_then_load_none() {
            let account = test_account(4);
            let store = KeychainCredentialStore::new(&account);
            let credentials = StoredCredentials::new("x".to_string(), None, vec![], None);
            store.save(credentials).await.expect("save");
            store.clear().await.expect("clear");
            assert!(store.load().await.expect("load").is_none());
        }

        #[tokio::test]
        async fn clear_when_empty_ok() {
            let account = test_account(5);
            let store = KeychainCredentialStore::new(&account);
            store.clear().await.expect("clear empty");
            store.clear().await.expect("clear again");
        }
    }
}
