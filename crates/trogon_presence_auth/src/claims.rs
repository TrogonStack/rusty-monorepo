use std::borrow::Cow;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use trogon_presence::{codec, ConnectionId, KeyError, PresenceKey};

use crate::connect_token::KeyId;
use crate::ids::{AuthEpoch, AuthRealmId, AuthSessionId, AuthVersion, TenantId};

const TOKEN_ID_MAX_BYTES: usize = 128;
const ACCOUNT_NAME_MAX_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    #[error("sub is not a valid presence key: {0}")]
    Subject(#[from] KeyError),
    #[error(
        "aud must be a NATS account name of 1 to {ACCOUNT_NAME_MAX_BYTES} bytes without whitespace, '.', '*' or '>'"
    )]
    Account,
    #[error("jti must be 1 to {TOKEN_ID_MAX_BYTES} bytes")]
    TokenId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(PresenceKey);

impl Subject {
    pub fn key(&self) -> &PresenceKey {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn token(&self) -> &str {
        self.0.token()
    }
}

impl TryFrom<String> for Subject {
    type Error = ClaimError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Ok(Self(PresenceKey::try_from(raw)?))
    }
}

impl From<PresenceKey> for Subject {
    fn from(key: PresenceKey) -> Self {
        Self(key)
    }
}

impl From<Subject> for String {
    fn from(subject: Subject) -> Self {
        subject.0.as_str().to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountName(String);

impl AccountName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AccountName {
    type Error = ClaimError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let valid = !raw.is_empty()
            && raw.len() <= ACCOUNT_NAME_MAX_BYTES
            && !raw.chars().any(|c| c.is_whitespace() || matches!(c, '.' | '*' | '>'));
        if !valid {
            return Err(ClaimError::Account);
        }
        Ok(Self(raw))
    }
}

impl std::str::FromStr for AccountName {
    type Err = ClaimError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<AccountName> for String {
    fn from(account: AccountName) -> Self {
        account.0
    }
}

impl fmt::Display for AccountName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenId(String);

impl TokenId {
    pub fn random() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            bytes,
        )))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn token(&self) -> Cow<'_, str> {
        codec::encode(self.0.as_bytes())
    }
}

impl TryFrom<String> for TokenId {
    type Error = ClaimError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > TOKEN_ID_MAX_BYTES {
            return Err(ClaimError::TokenId);
        }
        Ok(Self(raw))
    }
}

impl From<TokenId> for String {
    fn from(id: TokenId) -> Self {
        id.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixSeconds(u64);

impl UnixSeconds {
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    pub fn now() -> Self {
        Self::from(SystemTime::now())
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration.as_secs()))
    }

    pub fn saturating_sub(self, duration: Duration) -> Self {
        Self(self.0.saturating_sub(duration.as_secs()))
    }

    pub fn until(self, later: Self) -> Duration {
        Duration::from_secs(later.0.saturating_sub(self.0))
    }
}

impl From<SystemTime> for UnixSeconds {
    fn from(time: SystemTime) -> Self {
        Self(time.duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs()))
    }
}

impl fmt::Display for UnixSeconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectClaims {
    pub sub: Subject,
    pub tenant: TenantId,
    pub aud: AccountName,
    pub sid: AuthSessionId,
    pub cid: ConnectionId,
    pub auth_realm: AuthRealmId,
    pub auth_epoch: AuthEpoch,
    pub asv: AuthVersion,
    pub iat: UnixSeconds,
    pub exp: UnixSeconds,
    pub jti: TokenId,
    pub kid: KeyId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectIdentity {
    pub sub: Subject,
    pub tenant: TenantId,
    pub sid: AuthSessionId,
    pub cid: ConnectionId,
    pub auth_realm: AuthRealmId,
    pub auth_epoch: AuthEpoch,
    pub asv: AuthVersion,
    pub session_expires_at: UnixSeconds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_escapes_like_the_codec() -> Result<(), ClaimError> {
        let subject = Subject::try_from("ana@x.io".to_owned())?;
        assert_eq!(subject.token(), "ana=40x=2Eio");
        assert!(Subject::try_from(String::new()).is_err());
        Ok(())
    }

    #[test]
    fn account_name_rejects_subject_syntax() {
        for bad in ["", "a.b", "a*", ">", "a b"] {
            assert_eq!(AccountName::try_from(bad.to_owned()), Err(ClaimError::Account));
        }
        assert!(AccountName::try_from("APP".to_owned()).is_ok());
    }

    #[test]
    fn token_id_escapes_for_kv() -> Result<(), ClaimError> {
        assert_eq!(TokenId::try_from("a*b".to_owned())?.token(), "a=2Ab");
        Ok(())
    }
}
