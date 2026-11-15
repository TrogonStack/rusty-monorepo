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
#[path = "named_tests.rs"]
mod tests;
