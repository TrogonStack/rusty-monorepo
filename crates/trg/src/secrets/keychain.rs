//! macOS Keychain backend.
//!
//! One keychain generic-password item per [`SecretPath`]: the backend's
//! `service` is the item's service attribute and the path is its account
//! attribute. The item's payload is the JSON encoding of a [`SecretMap`], so a
//! path holding several keys still costs exactly one item.
//!
//! # Why we shell out to `/usr/bin/security` instead of using the `keyring` crate
//!
//! macOS's legacy file-based Keychain pins each item's ACL to the calling
//! process's **codesign identity** (its `cdhash`). `cargo build` produces a
//! fresh ad-hoc signature on every rebuild, so a binary built five minutes ago
//! looks like a *different* application to Keychain Services. The next read
//! pops a blocking GUI dialog ("X wants to use your `login` keychain"):
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
//! - <https://github.com/open-source-cooperative/keyring-rs/issues/272>:
//!   "Tauri with keyring - asking for permission?" Same root cause; maintainer
//!   confirms the ACL settings "aren't available via `keyring`" and recommends
//!   switching to the data-protection keychain (see next paragraph).
//! - <https://github.com/open-source-cooperative/keyring-rs/issues/23>:
//!   "macOS requests password twice for each `get_password`". Earliest report
//!   of the same symptom; closed with "instruct users to Always Allow".
//!
//! Their recommended workaround, switching to the data-protection (modern)
//! Keychain via `apple-native-keyring-store`'s `protected` feature, requires
//! a provisioning profile and is documented as unusable for command-line
//! tools in that crate's own module docs:
//! <https://docs.rs/apple-native-keyring-store/1.0.0/apple_native_keyring_store/protected/index.html>
//! (verbatim: *"Since command-line tools cannot be code-signed, there's not
//! much point in their using this module."*).
//!
//! `security-framework` likewise wraps only `SecKeychainAddGenericPassword`,
//! which doesn't take a `SecAccess` parameter, see
//! `os::macos::passwords.rs::add_generic_password` (passes `ptr::null_mut()`).
//! Recent access-control work in that crate targets the data-protection
//! keychain path only, not the legacy ACL we need:
//!
//! - <https://github.com/kornelski/rust-security-framework/pull/178>:
//!   "Support access control options" (data-protection keychain).
//! - <https://github.com/kornelski/rust-security-framework/pull/220>:
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

use std::path::PathBuf;

use tokio::process::Command;

use super::{SecretMap, SecretPath, SecretsError};

/// The keychain service attribute every item written by `trg` carries.
pub const DEFAULT_SERVICE: &str = "trg MCP Credentials";

const SECURITY_BIN: &str = "/usr/bin/security";

#[derive(Clone)]
pub struct KeychainBackend {
    service: String,
    /// The `security` executable to drive. Held as data rather than hardcoded
    /// at the call site so tests can drive a scripted one and pin the exact
    /// argv and the exact stderr tokens this backend keys off.
    bin: PathBuf,
}

impl KeychainBackend {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            bin: PathBuf::from(SECURITY_BIN),
        }
    }

    #[cfg(test)]
    fn with_bin(service: impl Into<String>, bin: impl AsRef<std::path::Path>) -> Self {
        Self {
            service: service.into(),
            bin: bin.as_ref().to_path_buf(),
        }
    }

    pub fn with_default_service() -> Self {
        Self::new(DEFAULT_SERVICE)
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        self.guard_platform()?;
        let out = self
            .run(&["find-generic-password", "-s", &self.service, "-a", path.as_str(), "-w"])
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("could not be found") {
                return Ok(None);
            }
            return Err(classify(&stderr, "find-generic-password"));
        }

        let payload = String::from_utf8_lossy(&out.stdout);
        let payload = payload.trim_end_matches('\n');
        SecretMap::from_json(payload)
            .map(Some)
            .map_err(|e| SecretsError::Malformed {
                path: path.clone(),
                cause: e.to_string(),
            })
    }

    pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
        self.guard_platform()?;
        let payload = map.to_json().map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: e.to_string(),
        })?;

        let out = self
            .run(&[
                "add-generic-password",
                "-U",
                "-A",
                "-s",
                &self.service,
                "-a",
                path.as_str(),
                "-w",
                &payload,
            ])
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(classify(&stderr, "add-generic-password"));
        }
        Ok(())
    }

    pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
        self.guard_platform()?;
        let out = self
            .run(&["delete-generic-password", "-s", &self.service, "-a", path.as_str()])
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("could not be found") {
                return Ok(());
            }
            return Err(classify(&stderr, "delete-generic-password"));
        }
        Ok(())
    }

    /// Enumerating a keychain service requires `dump-keychain`, which prompts
    /// for the keychain password once per item and defeats the whole point of
    /// the `-A` ACL above.
    pub async fn list(&self, _prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "keychain",
            op: "list",
        })
    }

    fn guard_platform(&self) -> Result<(), SecretsError> {
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            Err(SecretsError::Unavailable(
                "the keychain backend is available only on macOS".to_string(),
            ))
        }
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, SecretsError> {
        Command::new(&self.bin)
            .args(args)
            .output()
            .await
            .map_err(|e| SecretsError::Transport(format!("security {}: {e}", args[0])))
    }
}

fn classify(stderr: &str, op: &str) -> SecretsError {
    let stderr = stderr.trim();
    if stderr.contains("User interaction is not allowed") || stderr.contains("-25308") {
        return SecretsError::PermissionDenied(format!("keychain refused `{op}` without user interaction: {stderr}"));
    }
    SecretsError::Transport(format!("security {op} failed: {stderr}"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::secrets::SecretKey;
    use secrecy::{ExposeSecret, SecretString};

    /// A stand-in for `/usr/bin/security` that records its argv and answers
    /// with a fixed exit code, stdout and stderr.
    ///
    /// The backend under test is the production one driving a real child
    /// process, so what these tests pin down is the real argv and the real
    /// stderr tokens, not a re-description of them.
    struct StubSecurity {
        dir: tempfile::TempDir,
    }

    fn sh_quote(raw: &str) -> String {
        format!("'{}'", raw.replace('\'', r"'\''"))
    }

    impl StubSecurity {
        fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("security");
            let argv = dir.path().join("argv");
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\nprintf '%s' {stdout}\nprintf '%s' {stderr} >&2\nexit {exit}\n",
                argv = sh_quote(&argv.display().to_string()),
                stdout = sh_quote(stdout),
                stderr = sh_quote(stderr),
            );
            std::fs::write(&bin, script).expect("write stub");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

            Self { dir }
        }

        fn ok(stdout: &str) -> Self {
            Self::answering(0, stdout, "")
        }

        fn backend(&self) -> KeychainBackend {
            KeychainBackend::with_bin("svc", self.dir.path().join("security"))
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("argv"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    fn test_path(ns: u32) -> SecretPath {
        SecretPath::parse(&format!("trg-test-{}-{ns}", std::process::id())).expect("parse")
    }

    fn map_of(pairs: &[(&str, &str)]) -> SecretMap {
        let mut map = SecretMap::new();
        for (k, v) in pairs {
            map.insert(SecretKey::parse(k).expect("key"), SecretString::from(*v));
        }
        map
    }

    const NOT_FOUND: &str =
        "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.";

    #[tokio::test]
    async fn get_asks_for_the_password_of_this_service_and_account() {
        let stub = StubSecurity::ok("{\"credentials\":\"{}\"}\n");
        let map = stub
            .backend()
            .get(&SecretPath::parse("github").unwrap())
            .await
            .expect("get")
            .expect("some");

        assert_eq!(
            map.get(&SecretKey::parse("credentials").unwrap())
                .map(|v| v.expose_secret()),
            Some("{}")
        );
        assert_eq!(
            stub.argv(),
            ["find-generic-password", "-s", "svc", "-a", "github", "-w"]
        );
    }

    #[tokio::test]
    async fn get_of_an_absent_item_is_a_miss_not_an_error() {
        let stub = StubSecurity::answering(44, "", NOT_FOUND);
        assert!(stub
            .backend()
            .get(&SecretPath::parse("github").unwrap())
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn a_blocked_prompt_is_reported_as_a_permission_problem() {
        let stub = StubSecurity::answering(
            36,
            "",
            "security: SecKeychainSearchCopyNext: User interaction is not allowed.",
        );
        let err = stub
            .backend()
            .get(&SecretPath::parse("github").unwrap())
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::PermissionDenied(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_unrecognised_failure_stays_a_transport_error() {
        let stub = StubSecurity::answering(1, "", "security: the keychain is locked");
        let err = stub
            .backend()
            .get(&SecretPath::parse("github").unwrap())
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_payload_that_is_not_a_secret_map_is_malformed() {
        let stub = StubSecurity::ok("not json\n");
        let path = SecretPath::parse("github").unwrap();
        let err = stub.backend().get(&path).await.expect_err("should fail");

        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn set_creates_the_item_with_an_empty_trusted_application_list() {
        let stub = StubSecurity::ok("");
        stub.backend()
            .set(&SecretPath::parse("github").unwrap(), &map_of(&[("k", "v")]))
            .await
            .expect("set");

        assert_eq!(
            stub.argv(),
            [
                "add-generic-password",
                "-U",
                "-A",
                "-s",
                "svc",
                "-a",
                "github",
                "-w",
                "{\"k\":\"v\"}"
            ],
            "`-U -A` is what keeps a rebuilt binary from re-prompting"
        );
    }

    #[tokio::test]
    async fn delete_of_an_absent_item_is_idempotent() {
        let stub = StubSecurity::answering(44, "", NOT_FOUND);
        stub.backend()
            .delete(&SecretPath::parse("github").unwrap())
            .await
            .expect("delete");

        assert_eq!(stub.argv(), ["delete-generic-password", "-s", "svc", "-a", "github"]);
    }

    #[tokio::test]
    async fn a_failing_delete_that_is_not_a_miss_is_reported() {
        let stub = StubSecurity::answering(1, "", "security: the keychain is locked");
        let err = stub
            .backend()
            .delete(&SecretPath::parse("github").unwrap())
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_unspawnable_security_binary_is_a_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = KeychainBackend::with_bin("svc", dir.path().join("absent"));
        let err = backend
            .get(&SecretPath::parse("github").unwrap())
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn list_is_unsupported() {
        let stub = StubSecurity::ok("");
        assert!(matches!(
            stub.backend().list(None).await,
            Err(SecretsError::Unsupported { op: "list", .. })
        ));
    }

    /// Round-trips against the real login keychain.
    ///
    /// Ignored by default: the legacy file-based keychain serialises poorly
    /// under the parallelism of the full test binary, and a lost write there
    /// says nothing about this crate. Run with
    /// `cargo test -p trg --lib keychain::tests::live -- --ignored --test-threads=1`.
    #[tokio::test]
    #[ignore = "writes to the developer's real login keychain"]
    async fn live_set_get_delete_roundtrip() {
        let path = test_path(1);
        let backend = KeychainBackend::with_default_service();

        backend
            .set(&path, &map_of(&[("credentials", "{\"a\":1}")]))
            .await
            .expect("set");

        let loaded = backend.get(&path).await.expect("get").expect("some");
        assert_eq!(
            loaded
                .get(&SecretKey::parse("credentials").unwrap())
                .map(|v| v.expose_secret()),
            Some("{\"a\":1}")
        );

        backend
            .set(&path, &map_of(&[("credentials", "{\"a\":2}")]))
            .await
            .expect("overwrite");
        let loaded = backend.get(&path).await.expect("get").expect("some");
        assert_eq!(loaded.len(), 1);

        backend.delete(&path).await.expect("delete");
        assert!(backend.get(&path).await.expect("get").is_none());
        backend.delete(&path).await.expect("delete again");
    }
}
