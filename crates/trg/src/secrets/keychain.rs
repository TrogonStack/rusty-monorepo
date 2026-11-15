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

mod constants;
pub use constants::DEFAULT_SERVICE;
use constants::SECURITY_BIN;
use std::path::PathBuf;
use std::process::Stdio;

use secrecy::SecretString;
use tokio::process::Command;
use tracing::Instrument;

use super::telemetry::CliSpan;
use super::{BackendKind, SecretKey, SecretMap, SecretPath, SecretsError};
use crate::telemetry::propagation;

/// How a config var addresses one value in this backend.
///
/// A path names the keychain item, whose account attribute it becomes, and a
/// key names one field of the [`SecretMap`] that item's payload encodes.
///
/// A distinct type from [`super::OpenbaoReference`] even though both are
/// path-and-key shaped: the path lands on a service and account pair here and
/// on a mount, prefix and owner there, so they are the same spelling of two
/// different things.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeychainReference {
    path: SecretPath,
    key: SecretKey,
}

impl KeychainReference {
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

    #[cfg(all(test, target_os = "macos"))]
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

    /// Where this backend puts things, phrased for a person reading command
    /// output. It lives beside the service name it quotes so that renaming one
    /// cannot leave a copy of the other behind somewhere else in the tree.
    pub fn describe(&self) -> String {
        format!("the macOS Keychain (service `{}`)", self.service)
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The existing backend error model stores a display cause with separately redacted payloads; retaining typed sources requires a coordinated error-model migration"
        )
    )]
    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        self.guard_platform()?;
        let out = self
            .run(
                "find-generic-password",
                &["-s", &self.service, "-a", path.as_str(), "-w"],
            )
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("could not be found") {
                return Ok(None);
            }
            return Err(classify(&stderr, "find-generic-password"));
        }

        let payload = String::from_utf8_lossy(&out.stdout);
        let payload = payload.trim_end_matches('\n').to_string();
        SecretMap::from_json(&payload)
            .map(Some)
            .map_err(|e| SecretsError::Malformed {
                path: path.clone(),
                cause: e.to_string(),
                raw: Some(SecretString::from(payload)),
            })
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The existing backend error model stores a display cause with separately redacted payloads; retaining typed sources requires a coordinated error-model migration"
        )
    )]
    pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
        self.guard_platform()?;
        let payload = map.to_json().map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: e.to_string(),
            raw: None,
        })?;

        let out = self
            .run(
                "add-generic-password",
                &["-U", "-A", "-s", &self.service, "-a", path.as_str(), "-w", &payload],
            )
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
            .run("delete-generic-password", &["-s", &self.service, "-a", path.as_str()])
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

    /// Runs `security <operation> <args>` under a CLI client span that names
    /// the operation and none of the arguments, which carry the account and,
    /// for `add-generic-password`, the payload itself.
    async fn run(&self, operation: &'static str, args: &[&str]) -> Result<std::process::Output, SecretsError> {
        let call = CliSpan::start(BackendKind::Keychain, "security", operation);
        let mut command = Command::new(&self.bin);
        command
            .arg(operation)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        call.span().in_scope(|| propagation::inject_tokio_command(&mut command));

        let result = async {
            let child = command.spawn()?;
            call.spawned(child.id());
            child.wait_with_output().await
        }
        .instrument(call.span().clone())
        .await;

        match result {
            Ok(output) => {
                call.exited(&output.status);
                Ok(output)
            }
            Err(e) => {
                let error = SecretsError::Transport(format!("security {operation}: {e}"));
                call.failed(error.error_type());
                Err(error)
            }
        }
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
#[path = "keychain_tests.rs"]
mod tests;
