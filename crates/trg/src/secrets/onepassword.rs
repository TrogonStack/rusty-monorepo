//! 1Password backend, addressed through an already-authenticated `op` CLI.
//!
//! # Why we shell out to `op` instead of the Connect API or a service account
//!
//! Both of those need infrastructure or a token this backend would then have
//! to manage: a Connect server to run, or a service-account token to store
//! somewhere (which begs the question this backend exists to answer). `op`
//! sidesteps both — it's already signed in via the 1Password desktop app, the
//! same session a developer's shell already trusts, so shelling out reuses
//! that session with nothing new to authenticate or store. This mirrors why
//! [`super::keychain`] shells out to `/usr/bin/security` rather than pulling
//! in a keyring crate: the local, already-unlocked CLI is the whole point.
//!
//! # Addressing
//!
//! A [`SecretPath`] here is `"<vault>/<item>"` — the first `/`-segment names
//! the 1Password vault, the rest names the item title (rejoined with `/` if
//! it somehow contained one). A [`SecretKey`] is that item's field label.
//! `op item get <item> --vault <vault> --format json` already reveals
//! concealed field values in its JSON output with no `--reveal` flag needed
//! (that flag only affects the masked human-readable table format), so one
//! call yields the whole [`SecretMap`] for an item, same as a single OpenBao
//! or Keychain read yields every key stored at a path.
//!
//! # What this does not do
//!
//! `set`/`delete`/`list` are unsupported: the items this backend reads are
//! managed by hand, via the 1Password app or `op` CLI directly, not through
//! `trg secret put/delete/list`. This is a read path onto secrets that
//! already exist, not a place `trg` writes to.

use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use tokio::process::Command;

use super::{SecretKey, SecretMap, SecretPath, SecretsError};

/// How long an `op` invocation gets before it is treated as hung.
///
/// `op` normally answers in well under a second, but a stalled 1Password
/// desktop app or a Touch ID prompt nothing is watching for can otherwise
/// block `get`/`whoami` — and, through `whoami`, `trg doctor` — forever.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct OnePasswordBackend {
    account: Option<String>,
    /// The `op` executable to drive. Held as data rather than hardcoded at the
    /// call site so tests can drive a scripted one and pin the exact argv.
    bin: PathBuf,
    /// How long one `op` invocation gets; overridden in tests so a hung
    /// stub doesn't cost the suite real wall-clock time.
    timeout: Duration,
}

impl OnePasswordBackend {
    pub fn new(account: Option<String>) -> Self {
        Self {
            account,
            bin: PathBuf::from("op"),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_bin(account: Option<String>, bin: impl AsRef<std::path::Path>) -> Self {
        Self {
            account,
            bin: bin.as_ref().to_path_buf(),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        let (vault, item) = split(path)?;

        let mut args = vec!["item", "get", item, "--vault", vault, "--format", "json"];
        if let Some(account) = &self.account {
            args.push("--account");
            args.push(account);
        }
        let out = self.run(&args).await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("isn't an item in the") {
                return Ok(None);
            }
            return Err(SecretsError::Unavailable(format!(
                "op item get failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: format!("op item get did not return valid JSON: {e}"),
        })?;
        let fields = value
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| SecretsError::Malformed {
                path: path.clone(),
                cause: "op item get response has no `fields` array".to_string(),
            })?;

        let mut map = SecretMap::new();
        for field in fields {
            if field.get("purpose").and_then(|p| p.as_str()) == Some("NOTES") {
                continue;
            }
            let (Some(label), Some(value)) = (
                field.get("label").and_then(|l| l.as_str()),
                field.get("value").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let Ok(key) = SecretKey::parse(label) else {
                continue;
            };
            map.insert(key, SecretString::from(value.to_string()));
        }
        Ok(Some(map))
    }

    /// Confirm `op` is reachable and signed in, for `trg doctor`.
    ///
    /// There is no read this backend performs, such as OpenBao's health
    /// endpoint, that is meaningful without already naming an item — so this
    /// asks `op` about its own session instead, the same session every real
    /// `get` will ride on.
    pub async fn whoami(&self) -> Result<String, SecretsError> {
        let mut args = vec!["whoami", "--format", "json"];
        if let Some(account) = &self.account {
            args.push("--account");
            args.push(account);
        }
        let out = self.run(&args).await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "op whoami failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("op whoami did not return valid JSON: {e}")))?;
        let email = value.get("email").and_then(|v| v.as_str()).unwrap_or("unknown account");
        match value.get("url").and_then(|v| v.as_str()) {
            Some(url) if !url.is_empty() => Ok(format!("signed in as {email} ({url})")),
            _ => Ok(format!("signed in as {email}")),
        }
    }

    /// 1Password items are managed via the app or `op` CLI directly; `trg`
    /// only ever reads one that already exists.
    pub async fn set(&self, _path: &SecretPath, _map: &SecretMap) -> Result<(), SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "put",
        })
    }

    /// See [`Self::set`] — nothing here is `trg`'s to remove.
    pub async fn delete(&self, _path: &SecretPath) -> Result<(), SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "delete",
        })
    }

    /// See [`Self::set`] — enumerating would need a vault to scope to, which
    /// a bare prefix doesn't reliably carry, and there's no write side here to
    /// make listing useful anyway.
    pub async fn list(&self, _prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "list",
        })
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, SecretsError> {
        let child = Command::new(&self.bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SecretsError::Transport(format!("op {}: {e}", args.join(" "))))?;

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| SecretsError::Transport(format!("op {}: {e}", args.join(" ")))),
            Err(_) => Err(SecretsError::Transport(format!(
                "op {} timed out after {:?} — is the 1Password app unlocked?",
                args.join(" "),
                self.timeout
            ))),
        }
    }
}

fn split(path: &SecretPath) -> Result<(&str, &str), SecretsError> {
    match path.as_str().split_once('/') {
        Some((vault, item)) if !vault.is_empty() && !item.is_empty() => Ok((vault, item)),
        _ => Err(SecretsError::Malformed {
            path: path.clone(),
            cause: "a 1Password path needs `<vault>/<item>`, e.g. `Ops/deploy-keys`".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// A stand-in for `op` that records its argv and answers with a fixed
    /// exit code, stdout and stderr.
    ///
    /// The backend under test is the production one driving a real child
    /// process, so what these tests pin down is the real argv this backend
    /// sends and the real stderr tokens it keys off, not a re-description of
    /// them.
    struct StubOp {
        dir: tempfile::TempDir,
    }

    fn sh_quote(raw: &str) -> String {
        format!("'{}'", raw.replace('\'', r"'\''"))
    }

    impl StubOp {
        fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("op");
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

        /// A stand-in that never returns on its own, for exercising the
        /// timeout — as if `op` were blocked on an unlock prompt no one is
        /// watching for.
        fn hanging() -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("op");
            std::fs::write(&bin, "#!/bin/sh\nsleep 300\n").expect("write stub");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

            Self { dir }
        }

        fn backend(&self, account: Option<&str>) -> OnePasswordBackend {
            OnePasswordBackend::with_bin(account.map(str::to_string), self.dir.path().join("op"))
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("argv"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    const ITEM_JSON: &str = r#"{
        "title": "deploy-keys",
        "vault": { "id": "v1", "name": "Ops" },
        "category": "SECURE_NOTE",
        "fields": [
            { "label": "notesPlain", "type": "STRING", "purpose": "NOTES", "value": "some notes" },
            { "label": "TOKEN_A", "type": "CONCEALED", "value": "sk-ant-fake-value" },
            { "label": "TOKEN_B", "type": "CONCEALED", "value": "sk-ant-fake-value-2" }
        ]
    }"#;

    const NOT_AN_ITEM: &str =
        "[ERROR] \"missing\" isn't an item in the \"Ops\" vault. Specify the item with its UUID, name, or domain.";
    const NOT_A_VAULT: &str = "[ERROR] \"Ops\" isn't a vault in this account. Specify the vault with its ID or name.";

    #[tokio::test]
    async fn get_returns_every_field_but_the_note_body() {
        let stub = StubOp::ok(ITEM_JSON);
        let map = stub
            .backend(None)
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect("get")
            .expect("some");

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&SecretKey::parse("TOKEN_A").unwrap())
                .map(|v| v.expose_secret()),
            Some("sk-ant-fake-value")
        );
        assert!(!map.contains_key(&SecretKey::parse("notesPlain").unwrap()));
        assert_eq!(
            stub.argv(),
            ["item", "get", "deploy-keys", "--vault", "Ops", "--format", "json"]
        );
    }

    #[tokio::test]
    async fn an_account_is_passed_through_when_configured() {
        let stub = StubOp::ok(ITEM_JSON);
        stub.backend(Some("my.1password.com"))
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect("get");

        assert_eq!(
            stub.argv(),
            [
                "item",
                "get",
                "deploy-keys",
                "--vault",
                "Ops",
                "--format",
                "json",
                "--account",
                "my.1password.com"
            ]
        );
    }

    #[tokio::test]
    async fn get_of_a_missing_item_is_a_miss_not_an_error() {
        let stub = StubOp::answering(1, "", NOT_AN_ITEM);
        assert!(stub
            .backend(None)
            .get(&SecretPath::parse("Ops/missing").unwrap())
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn get_against_a_vault_that_does_not_exist_is_an_error() {
        let stub = StubOp::answering(1, "", NOT_A_VAULT);
        let err = stub
            .backend(None)
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_payload_that_is_not_valid_json_is_malformed() {
        let stub = StubOp::ok("not json");
        let err = stub
            .backend(None)
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_path_without_a_vault_and_item_is_rejected_before_spawning_anything() {
        let stub = StubOp::ok(ITEM_JSON);
        let err = stub
            .backend(None)
            .get(&SecretPath::parse("no-slash-here").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
        assert!(stub.argv().is_empty(), "should not have spawned op");
    }

    #[tokio::test]
    async fn set_delete_and_list_are_unsupported_and_spawn_nothing() {
        let stub = StubOp::ok(ITEM_JSON);
        let backend = stub.backend(None);
        let path = SecretPath::parse("Ops/deploy-keys").unwrap();

        assert!(matches!(
            backend.set(&path, &SecretMap::new()).await,
            Err(SecretsError::Unsupported { op: "put", .. })
        ));
        assert!(matches!(
            backend.delete(&path).await,
            Err(SecretsError::Unsupported { op: "delete", .. })
        ));
        assert!(matches!(
            backend.list(None).await,
            Err(SecretsError::Unsupported { op: "list", .. })
        ));
        assert!(stub.argv().is_empty(), "should not have spawned op");
    }

    #[tokio::test]
    async fn whoami_reports_the_signed_in_account() {
        let stub = StubOp::ok(r#"{"url":"my.1password.com","email":"someone@example.com"}"#);
        let detail = stub.backend(None).whoami().await.expect("whoami");
        assert_eq!(detail, "signed in as someone@example.com (my.1password.com)");
        assert_eq!(stub.argv(), ["whoami", "--format", "json"]);
    }

    #[tokio::test]
    async fn whoami_when_signed_out_is_an_error() {
        let stub = StubOp::answering(1, "", "[ERROR] you are not currently signed in");
        let err = stub.backend(None).whoami().await.expect_err("should fail");
        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_hung_op_is_reported_as_timed_out_rather_than_waited_on_forever() {
        let stub = StubOp::hanging();
        let backend = stub.backend(None).with_timeout(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let err = backend.whoami().await.expect_err("should time out");

        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait anywhere near the stub's 300s sleep"
        );
    }

    #[tokio::test]
    async fn an_unspawnable_op_binary_is_a_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = OnePasswordBackend::with_bin(None, dir.path().join("absent"));
        let err = backend
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }
}
