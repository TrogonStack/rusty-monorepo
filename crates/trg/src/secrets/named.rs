//! `[secrets.vars.<name>]`: a secret declared once and read by name.
//!
//! A table here is the same inline table an env var or a `--fallback` is
//! written as, so a value reads the same way wherever it is named. What this
//! adds is only the name, which lets a script say `trg secret get --name x`
//! instead of spelling out the address and every fallback as flags.

use std::fmt;

use serde::Deserialize;

use crate::config::{RawSecretVar, RawVarSource, SecretVarError, WithFallbacks};

/// The name a `[secrets.vars.<name>]` table is declared under.
///
/// Limited to what a TOML bare key holds, so a secret can always be written as
/// `[secrets.vars.<name>]` without quoting and typed on the command line as it
/// is written in the file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct SecretName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretNameError {
    #[error("a secret name must not be empty")]
    Empty,

    #[error("secret name `{0}` may hold only ASCII letters, digits, `-` and `_`")]
    Characters(String),
}

impl SecretName {
    pub fn parse(raw: &str) -> Result<Self, SecretNameError> {
        if raw.is_empty() {
            return Err(SecretNameError::Empty);
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(SecretNameError::Characters(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretName {
    type Error = SecretNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl std::str::FromStr for SecretName {
    type Err = SecretNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One `[secrets.vars.<name>]` table as the file spelled it.
///
/// Exactly the secret shapes a var can take: one address, or one address with
/// `fallbacks`. A literal or an env var is not a secret and has no place here.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
#[serde(
    expecting = "a secret table such as `{ backend = \"<name>\", path = \"<path>\", key = \"<key>\" }` \
                     or `{ backend = \"<name>\", ref = \"op://<vault>/<item>/<field>\" }`, \
                     optionally with `fallbacks = [ <secret tables> ]` whose entries cannot carry `fallbacks` of their own"
)]
pub enum NamedSecretConfig {
    WithFallbacks(Box<WithFallbacks<RawSecretVar>>),
    Secret(RawSecretVar),
}

impl NamedSecretConfig {
    pub(super) fn into_source(self) -> RawVarSource {
        match self {
            Self::WithFallbacks(chain) => RawVarSource::SecretWithFallbacks(chain),
            Self::Secret(raw) => RawVarSource::Secret(raw),
        }
    }
}

/// Why a name did not lead to a read.
#[derive(Debug, thiserror::Error)]
pub enum NamedSecretError {
    #[error("no secret named `{name}` is declared; declared: {declared}")]
    Unknown { name: SecretName, declared: String },

    #[error("no secret named `{name}` is declared; add a `[secrets.vars.{name}]` section")]
    NoneDeclared { name: SecretName },

    #[error("{0}")]
    Var(#[from] SecretVarError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretRead;
    use crate::secrets::config::SecretsSection;

    fn resolve_all(
        secrets: &SecretsSection,
    ) -> Result<std::collections::BTreeMap<SecretName, SecretRead>, SecretVarError> {
        secrets.resolve_named_all()
    }

    fn resolve_named(secrets: &SecretsSection, name: &SecretName) -> Result<SecretRead, NamedSecretError> {
        secrets.resolve_named(name)
    }

    fn section(vars: &str) -> SecretsSection {
        toml::from_str(&format!(
            r#"
            [backends.homelab]
            kind = "openbao"
            addr = "https://bao.example.com"
            mount = "kv"
            path_prefix = "trg"
            owner = "me"
            token_file = "~/.t"

            [backends.op]
            kind = "onepassword"
            account = "my.1password.com"

            {vars}
            "#
        ))
        .expect("section")
    }

    fn name(raw: &str) -> SecretName {
        SecretName::parse(raw).unwrap()
    }

    const SIGNING: &str = r#"
        [vars.signing-key]
        backend = "homelab"
        path = "git/signing"
        key = "SSH_PRIVATE_KEY"
        fallbacks = [ { backend = "op", ref = "op://Private/Signing/private key?ssh-format=openssh" } ]
    "#;

    #[test]
    fn a_name_is_what_a_toml_bare_key_holds() {
        assert!(SecretName::parse("git-signing_key2").is_ok());
        assert_eq!(SecretName::parse(""), Err(SecretNameError::Empty));
        assert!(matches!(SecretName::parse("a b"), Err(SecretNameError::Characters(_))));
        assert!(matches!(SecretName::parse("a.b"), Err(SecretNameError::Characters(_))));
    }

    #[test]
    fn a_table_name_that_is_not_a_secret_name_fails_to_parse() {
        let err =
            toml::from_str::<SecretsSection>("[vars.\"a b\"]\nbackend = \"x\"\npath = \"p\"\nkey = \"k\"").unwrap_err();
        assert!(err.to_string().contains("secret name"), "{err}");
    }

    #[test]
    fn a_named_secret_with_fallbacks_resolves_to_a_read_in_order() {
        let read = resolve_named(&section(SIGNING), &name("signing-key")).unwrap();
        assert_eq!(read.primary().backend(), "homelab");
        assert_eq!(read.fallbacks().len(), 1);
        assert_eq!(read.fallbacks()[0].backend(), "op");
    }

    #[test]
    fn a_named_secret_without_fallbacks_is_a_single_address() {
        let read = resolve_named(
            &section("[vars.token]\nbackend = \"homelab\"\npath = \"p\"\nkey = \"k\""),
            &name("token"),
        )
        .unwrap();
        assert!(read.fallbacks().is_empty());
    }

    #[test]
    fn every_declared_secret_resolves_at_load() {
        let reads = resolve_all(&section(SIGNING)).unwrap();
        assert_eq!(reads.len(), 1);
    }

    #[test]
    fn empty_fallbacks_are_refused() {
        let s = section("[vars.t]\nbackend = \"homelab\"\npath = \"p\"\nkey = \"k\"\nfallbacks = []");
        let err = resolve_all(&s).unwrap_err();
        assert!(matches!(err, SecretVarError::EmptyFallbacks { .. }), "{err}");
        assert!(err.to_string().contains("[secrets.vars.t]"), "{err}");
    }

    #[test]
    fn a_fallback_cannot_carry_fallbacks() {
        let err = toml::from_str::<SecretsSection>(
            r#"[vars.t]
            backend = "a"
            path = "p"
            key = "k"
            fallbacks = [ { backend = "b", path = "p", key = "k", fallbacks = [] } ]"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("fallbacks"), "{err}");
    }

    #[test]
    fn a_literal_or_env_table_is_not_a_secret() {
        assert!(toml::from_str::<SecretsSection>("[vars]\nt = \"literal\"").is_err());
        assert!(toml::from_str::<SecretsSection>("[vars.t]\nenv = \"HOME\"").is_err());
    }

    #[test]
    fn an_unknown_backend_fails_at_load() {
        let s = section("[vars.t]\nbackend = \"ghost\"\npath = \"p\"\nkey = \"k\"");
        let err = resolve_all(&s).unwrap_err();
        assert!(matches!(err, SecretVarError::UnknownBackend { .. }), "{err}");
    }

    #[test]
    fn an_unknown_fallback_backend_fails_at_load() {
        let s = section(
            "[vars.t]\nbackend = \"homelab\"\npath = \"p\"\nkey = \"k\"\nfallbacks = [ { backend = \"ghost\", path = \"p\", key = \"k\" } ]",
        );
        assert!(resolve_all(&s).is_err());
    }

    #[test]
    fn the_wrong_vocabulary_for_a_backend_kind_is_refused() {
        let s = section("[vars.t]\nbackend = \"op\"\npath = \"p\"\nkey = \"k\"");
        let err = resolve_all(&s).unwrap_err();
        assert!(matches!(err, SecretVarError::NeedsReference { .. }), "{err}");
    }

    #[test]
    fn an_unknown_name_lists_the_declared_names_and_nothing_else() {
        let err = resolve_named(&section(SIGNING), &name("nope")).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, NamedSecretError::Unknown { .. }), "{msg}");
        assert!(msg.contains("declared: signing-key"), "{msg}");
        assert!(
            !msg.contains("SSH_PRIVATE_KEY") && !msg.contains("git/signing"),
            "{msg}"
        );
    }

    #[test]
    fn asking_when_none_are_declared_says_how_to_declare_one() {
        let err = resolve_named(&section(""), &name("nope")).unwrap_err();
        assert!(err.to_string().contains("[secrets.vars.nope]"), "{err}");
    }
}
