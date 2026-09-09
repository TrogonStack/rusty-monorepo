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
//! # Why the account is required
//!
//! A vault name is only unique *within* an account, and `op` will happily sign
//! a developer into several at once. Left to itself it resolves a bare
//! `--vault Ops` against whichever account it considers the default, which
//! is machine-local state — not something the config says — so the same
//! `[secrets.backends.<name>]` on two machines can address two different
//! vaults, and a personal account shadowing a work vault name is a silent
//! misread rather than an error. [`OpAccount`] is therefore mandatory and
//! every invocation carries `--account`: which account a path resolves against
//! is part of the address, and addresses belong in the config.
//!
//! # Why `op whoami` is not the health probe
//!
//! `op whoami` answers only for a session `op signin` (or a service account)
//! established. Under the desktop-app integration this backend is built
//! around it reports `account is not signed in` while every real read
//! succeeds, which made `trg doctor` call a perfectly healthy backend broken.
//! [`OnePasswordBackend::current_account`] asks `op account get` instead: it
//! needs the same live session an `item get` needs, so it fails exactly when
//! reads would.
//!
//! # What this does not do
//!
//! `set`/`delete`/`list` are unsupported: the items this backend reads are
//! managed by hand, via the 1Password app or `op` CLI directly, not through
//! `trg secret put/delete/list`. This is a read path onto secrets that
//! already exist, not a place `trg` writes to.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use tokio::process::Command;

use super::{SecretKey, SecretMap, SecretPath, SecretsError};

/// How long an `op` invocation gets before it is treated as hung.
///
/// `op` normally answers in well under a second, but a stalled 1Password
/// desktop app or a Touch ID prompt nothing is watching for can otherwise
/// block `get`/`current_account` — and, through the latter, `trg doctor` —
/// forever.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("1Password account must not be empty")]
    Empty,

    /// `op` would read this as a flag rather than as the account to use, and
    /// report something about an unknown option instead of about the config.
    #[error("1Password account `{0}` must not start with `-`")]
    Dash(String),
}

/// Which signed-in `op` account a backend addresses.
///
/// Structurally validated only: `op --account` accepts a sign-in address, an
/// email, a user UUID or an account UUID, and which of those a value is can
/// only be settled by asking `op` (see [`OnePasswordBackend::accounts`]), not
/// by looking at the string.
#[derive(Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct OpAccount(String);

impl OpAccount {
    pub fn parse(raw: &str) -> Result<Self, AccountError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(AccountError::Empty);
        }
        if raw.starts_with('-') {
            return Err(AccountError::Dash(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `signed_in` is the account this addresses.
    ///
    /// Matched over every form `op --account` accepts, because the config may
    /// legitimately carry any of them and refusing to recognise the one a
    /// developer chose would report a correct config as pointing nowhere.
    ///
    /// That includes the sign-in subdomain on its own — `my` for
    /// `my.1password.com` — which `op` accepts and which its own docs use, but
    /// which `op account list` never prints as a field of its own.
    fn addresses(&self, signed_in: &SignedInAccount) -> bool {
        let want = self.0.as_str();
        [
            signed_in.url.as_str(),
            signed_in.subdomain(),
            signed_in.email.as_str(),
            signed_in.user_uuid.as_str(),
            signed_in.account_uuid.as_str(),
        ]
        .into_iter()
        .any(|form| !form.is_empty() && form.eq_ignore_ascii_case(want))
    }
}

impl TryFrom<String> for OpAccount {
    type Error = AccountError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for OpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for OpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpAccount({:?})", self.0)
    }
}

/// One row of `op account list` — an account `op` on this machine can reach.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SignedInAccount {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub user_uuid: String,
    #[serde(default)]
    pub account_uuid: String,
}

impl SignedInAccount {
    /// The sign-in subdomain, `my` out of `my.1password.com`.
    ///
    /// Derived rather than read: `op account list` reports the full `url` and
    /// no separate field for this, though `--account` takes it.
    fn subdomain(&self) -> &str {
        self.url.split('.').next().unwrap_or_default()
    }
}

impl fmt::Display for SignedInAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.email.as_str(), self.url.as_str()) {
            ("", "") => f.write_str(&self.account_uuid),
            ("", url) => f.write_str(url),
            (email, "") => f.write_str(email),
            (email, url) => write!(f, "{email} ({url})"),
        }
    }
}

/// `op account get` — what the addressed account looks like to a live session.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CurrentAccount {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
}

#[derive(Clone)]
pub struct OnePasswordBackend {
    account: OpAccount,
    /// The `op` executable to drive. Held as data rather than hardcoded at the
    /// call site so tests can drive a scripted one and pin the exact argv.
    bin: PathBuf,
    /// How long one `op` invocation gets; overridden in tests so a hung
    /// stub doesn't cost the suite real wall-clock time.
    timeout: Duration,
}

impl OnePasswordBackend {
    pub fn new(account: OpAccount) -> Self {
        Self {
            account,
            bin: PathBuf::from("op"),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_bin(account: &str, bin: impl AsRef<std::path::Path>) -> Self {
        Self {
            account: OpAccount::parse(account).expect("test account"),
            bin: bin.as_ref().to_path_buf(),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn account(&self) -> &OpAccount {
        &self.account
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        let (vault, item) = split(path)?;

        let out = self
            .run(&[
                "item",
                "get",
                item,
                "--vault",
                vault,
                "--format",
                "json",
                "--account",
                self.account.as_str(),
            ])
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.contains("isn't an item in the") {
                return Ok(None);
            }
            // `op` names neither the account it used nor the vaults that
            // account does have, so an address aimed at the wrong one of
            // several signed-in accounts otherwise reads as the vault having
            // vanished.
            let hint = if stderr.contains("isn't a vault in this account") {
                format!(" (account `{}`)", self.account)
            } else {
                String::new()
            };
            return Err(SecretsError::Unavailable(format!("op item get failed: {stderr}{hint}")));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| SecretsError::Malformed {
            path: path.clone(),
            cause: format!("op item get did not return valid JSON: {e}"),
            raw: None,
        })?;
        let fields = value
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| SecretsError::Malformed {
                path: path.clone(),
                cause: "op item get response has no `fields` array".to_string(),
                raw: None,
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

    /// Every account `op` on this machine can reach, for `trg doctor` to
    /// resolve the configured [`OpAccount`] against.
    ///
    /// Reads local `op` state, so it answers whether or not anything is
    /// unlocked — which is the point: "you named an account `op` has never
    /// heard of" and "that account is locked" are different problems with
    /// different remedies, and conflating them sends people to `op signin`
    /// for a typo.
    pub async fn accounts(&self) -> Result<Vec<SignedInAccount>, SecretsError> {
        let out = self.run(&["account", "list", "--format", "json"]).await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "op account list failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("op account list did not return valid JSON: {e}")))
    }

    /// Confirm the addressed account has a session that can actually read.
    ///
    /// See the module docs on why this is `op account get` and not
    /// `op whoami`.
    pub async fn current_account(&self) -> Result<CurrentAccount, SecretsError> {
        let out = self
            .run(&["account", "get", "--format", "json", "--account", self.account.as_str()])
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "op account get failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("op account get did not return valid JSON: {e}")))
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

/// The account `filter` addresses, if `op` knows it.
pub fn resolve<'a>(filter: &OpAccount, known: &'a [SignedInAccount]) -> Option<&'a SignedInAccount> {
    known.iter().find(|a| filter.addresses(a))
}

fn split(path: &SecretPath) -> Result<(&str, &str), SecretsError> {
    match path.as_str().split_once('/') {
        Some((vault, item)) if !vault.is_empty() && !item.is_empty() => Ok((vault, item)),
        _ => Err(SecretsError::Malformed {
            path: path.clone(),
            cause: "a 1Password path needs `<vault>/<item>`, e.g. `Ops/deploy-keys`".to_string(),
            raw: None,
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
            Self::scripted(&format!(
                "printf '%s' {stdout}\nprintf '%s' {stderr} >&2\nexit {exit}\n",
                stdout = sh_quote(stdout),
                stderr = sh_quote(stderr),
            ))
        }

        fn ok(stdout: &str) -> Self {
            Self::answering(0, stdout, "")
        }

        /// A stand-in that answers per `op` subcommand, for the probes that
        /// make more than one call.
        fn routing(routes: &[(&str, i32, &str)]) -> Self {
            let mut body = String::from("case \"$1 $2\" in\n");
            for (subcommand, exit, stdout) in routes {
                body.push_str(&format!(
                    "  {subcommand}) printf '%s' {stdout}; exit {exit};;\n",
                    subcommand = sh_quote(subcommand),
                    stdout = sh_quote(stdout),
                ));
            }
            body.push_str("  *) printf 'unrouted: %s\\n' \"$*\" >&2; exit 127;;\nesac\n");
            Self::scripted(&body)
        }

        fn scripted(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("op");
            let argv = dir.path().join("argv");
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\n{body}",
                argv = sh_quote(&argv.display().to_string()),
            );
            std::fs::write(&bin, script).expect("write stub");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

            Self { dir }
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

        fn backend(&self) -> OnePasswordBackend {
            OnePasswordBackend::with_bin(ACCOUNT, self.dir.path().join("op"))
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("argv"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    const ACCOUNT: &str = "my.1password.com";

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

    const ACCOUNTS_JSON: &str = r#"[
        { "url": "my.1password.com", "email": "someone@example.com", "user_uuid": "U1", "account_uuid": "A1" },
        { "url": "team-acme.1password.com", "email": "someone@acme.test", "user_uuid": "U2", "account_uuid": "A2" }
    ]"#;

    const CURRENT_JSON: &str = r#"{ "id": "A1", "name": "Someone's Family", "domain": "my", "state": "ACTIVE" }"#;

    const NOT_AN_ITEM: &str =
        "[ERROR] \"missing\" isn't an item in the \"Ops\" vault. Specify the item with its UUID, name, or domain.";
    const NOT_A_VAULT: &str = "[ERROR] \"Ops\" isn't a vault in this account. Specify the vault with its ID or name.";

    #[test]
    fn an_account_is_rejected_when_it_is_empty_or_would_read_as_a_flag() {
        assert!(matches!(OpAccount::parse(""), Err(AccountError::Empty)));
        assert!(matches!(OpAccount::parse("   "), Err(AccountError::Empty)));
        assert!(matches!(OpAccount::parse("--account"), Err(AccountError::Dash(_))));
        assert_eq!(OpAccount::parse("  my.1password.com ").unwrap().as_str(), ACCOUNT);
    }

    #[test]
    fn an_account_resolves_by_any_form_op_itself_accepts() {
        let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

        for form in [
            "my.1password.com",
            "MY.1Password.com",
            "someone@example.com",
            "U1",
            "A1",
        ] {
            let found = resolve(&OpAccount::parse(form).unwrap(), &known);
            assert_eq!(found.map(|a| a.account_uuid.as_str()), Some("A1"), "{form}");
        }
        assert!(resolve(&OpAccount::parse("nope.1password.com").unwrap(), &known).is_none());
    }

    /// `op --account my` reads fine, so a config saying `my` is correct and
    /// must not be reported as naming an account `op` has never heard of.
    #[test]
    fn an_account_resolves_by_the_bare_sign_in_subdomain() {
        let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

        for (form, want) in [("my", "A1"), ("MY", "A1"), ("team-acme", "A2")] {
            let found = resolve(&OpAccount::parse(form).unwrap(), &known);
            assert_eq!(found.map(|a| a.account_uuid.as_str()), Some(want), "{form}");
        }
        assert!(resolve(&OpAccount::parse("team-other").unwrap(), &known).is_none());
    }

    /// An account row missing a field must not turn every lookup into a match
    /// on the empty string.
    #[test]
    fn an_account_with_blank_fields_matches_only_what_it_does_carry() {
        let known = vec![SignedInAccount {
            url: String::new(),
            email: String::new(),
            user_uuid: String::new(),
            account_uuid: "A9".to_string(),
        }];

        assert!(resolve(&OpAccount::parse("my.1password.com").unwrap(), &known).is_none());
        assert_eq!(
            resolve(&OpAccount::parse("A9").unwrap(), &known).map(|a| a.account_uuid.as_str()),
            Some("A9")
        );
    }

    #[tokio::test]
    async fn get_returns_every_field_but_the_note_body() {
        let stub = StubOp::ok(ITEM_JSON);
        let map = stub
            .backend()
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
    }

    #[tokio::test]
    async fn every_read_carries_the_configured_account() {
        let stub = StubOp::ok(ITEM_JSON);
        stub.backend()
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
                ACCOUNT
            ]
        );
    }

    #[tokio::test]
    async fn get_of_a_missing_item_is_a_miss_not_an_error() {
        let stub = StubOp::answering(1, "", NOT_AN_ITEM);
        assert!(stub
            .backend()
            .get(&SecretPath::parse("Ops/missing").unwrap())
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn a_vault_missing_from_the_account_names_the_account_that_was_asked() {
        let stub = StubOp::answering(1, "", NOT_A_VAULT);
        let err = stub
            .backend()
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
        assert!(err.to_string().contains(ACCOUNT), "{err}");
    }

    #[tokio::test]
    async fn a_payload_that_is_not_valid_json_is_malformed() {
        let stub = StubOp::ok("not json");
        let err = stub
            .backend()
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_path_without_a_vault_and_item_is_rejected_before_spawning_anything() {
        let stub = StubOp::ok(ITEM_JSON);
        let err = stub
            .backend()
            .get(&SecretPath::parse("no-slash-here").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
        assert!(stub.argv().is_empty(), "should not have spawned op");
    }

    #[tokio::test]
    async fn set_delete_and_list_are_unsupported_and_spawn_nothing() {
        let stub = StubOp::ok(ITEM_JSON);
        let backend = stub.backend();
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
    async fn accounts_lists_what_op_can_reach_without_naming_one() {
        let stub = StubOp::routing(&[("account list", 0, ACCOUNTS_JSON)]);
        let known = stub.backend().accounts().await.expect("accounts");

        assert_eq!(known.len(), 2);
        assert_eq!(known[0].email, "someone@example.com");
        assert_eq!(stub.argv(), ["account", "list", "--format", "json"]);
    }

    #[tokio::test]
    async fn the_session_probe_asks_op_about_the_configured_account() {
        let stub = StubOp::routing(&[("account get", 0, CURRENT_JSON)]);
        let current = stub.backend().current_account().await.expect("current");

        assert_eq!(current.id, "A1");
        assert_eq!(current.state, "ACTIVE");
        assert_eq!(
            stub.argv(),
            ["account", "get", "--format", "json", "--account", ACCOUNT]
        );
    }

    #[tokio::test]
    async fn a_locked_or_signed_out_account_makes_the_session_probe_fail() {
        let stub = StubOp::answering(1, "", "[ERROR] account is not signed in");
        let err = stub.backend().current_account().await.expect_err("should fail");
        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_hung_op_is_reported_as_timed_out_rather_than_waited_on_forever() {
        let stub = StubOp::hanging();
        let backend = stub.backend().with_timeout(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let err = backend.current_account().await.expect_err("should time out");

        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait anywhere near the stub's 300s sleep"
        );
    }

    #[tokio::test]
    async fn an_unspawnable_op_binary_is_a_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = OnePasswordBackend::with_bin(ACCOUNT, dir.path().join("absent"));
        let err = backend
            .get(&SecretPath::parse("Ops/deploy-keys").unwrap())
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }
}
