use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use nkeys::{KeyPair, KeyPairType};
use serde::{Deserialize, Serialize};
use trogon_presence::codec;

use crate::claims::AccountName;

const TENANT_MAX_BYTES: usize = 128;
const SESSION_MAX_BYTES: usize = 128;
const REALM_MAX_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("tenant id must be 1 to {TENANT_MAX_BYTES} bytes")]
    Tenant,
    #[error("auth session id must be 1 to {SESSION_MAX_BYTES} bytes")]
    Session,
    #[error("auth realm id must be 1 to {REALM_MAX_BYTES} bytes of [A-Za-z0-9_-]")]
    Realm,
    #[error("server key must be a server public NKey (N...)")]
    ServerNkey,
    #[error("user key must be a user public NKey (U...)")]
    UserNkey,
    #[error("tenant {0} is registered twice")]
    DuplicateTenant(TenantId),
    #[error("tenant mapping must be <tenant>=<account>")]
    TenantMapping,
}

macro_rules! bounded_text {
    ($name:ident, $max:expr, $error:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn token(&self) -> Cow<'_, str> {
                codec::encode(self.0.as_bytes())
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(raw: String) -> Result<Self, Self::Error> {
                if raw.is_empty() || raw.len() > $max {
                    return Err($error);
                }
                Ok(Self(raw))
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from(s.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

bounded_text!(TenantId, TENANT_MAX_BYTES, IdError::Tenant);
bounded_text!(AuthSessionId, SESSION_MAX_BYTES, IdError::Session);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AuthRealmId(String);

impl AuthRealmId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AuthRealmId {
    type Error = IdError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let valid = !raw.is_empty()
            && raw.len() <= REALM_MAX_BYTES
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if !valid {
            return Err(IdError::Realm);
        }
        Ok(Self(raw))
    }
}

impl FromStr for AuthRealmId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<AuthRealmId> for String {
    fn from(value: AuthRealmId) -> Self {
        value.0
    }
}

impl fmt::Display for AuthRealmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! counter {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub const INITIAL: Self = Self(1);

            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub fn get(self) -> u64 {
                self.0
            }

            pub fn next(self) -> Option<Self> {
                self.0.checked_add(1).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

counter!(AuthEpoch);
counter!(AuthVersion);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrokerClientId(u64);

impl BrokerClientId {
    pub const fn new(cid: u64) -> Self {
        Self(cid)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BrokerClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! public_nkey {
    ($name:ident, $kind:pat, $error:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(raw: String) -> Result<Self, Self::Error> {
                let pair = KeyPair::from_public_key(&raw).map_err(|_| $error)?;
                if !matches!(pair.key_pair_type(), $kind) {
                    return Err($error);
                }
                Ok(Self(raw))
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from(s.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

public_nkey!(ServerNkey, KeyPairType::Server, IdError::ServerNkey);
public_nkey!(UserNkey, KeyPairType::User, IdError::UserNkey);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantMapping {
    pub tenant: TenantId,
    pub account: AccountName,
}

impl FromStr for TenantMapping {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (tenant, account) = s.split_once('=').ok_or(IdError::TenantMapping)?;
        Ok(Self {
            tenant: tenant.parse()?,
            account: account.parse().map_err(|_| IdError::TenantMapping)?,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct TenantRegistry(HashMap<TenantId, AccountName>);

impl TenantRegistry {
    pub fn new(mappings: impl IntoIterator<Item = TenantMapping>) -> Result<Self, IdError> {
        let mut accounts = HashMap::new();
        for mapping in mappings {
            if accounts.insert(mapping.tenant.clone(), mapping.account).is_some() {
                return Err(IdError::DuplicateTenant(mapping.tenant));
            }
        }
        Ok(Self(accounts))
    }

    pub fn account(&self, tenant: &TenantId) -> Option<&AccountName> {
        self.0.get(tenant)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_and_session_escape_for_kv() -> Result<(), IdError> {
        assert_eq!(TenantId::try_from("acme.io".to_owned())?.token(), "acme=2Eio");
        assert_eq!(AuthSessionId::try_from("s*1".to_owned())?.token(), "s=2A1");
        assert!(TenantId::try_from(String::new()).is_err());
        Ok(())
    }

    #[test]
    fn nkeys_are_typed() {
        let server = KeyPair::new_server().public_key();
        let user = KeyPair::new_user().public_key();
        assert!(server.parse::<ServerNkey>().is_ok());
        assert_eq!(user.parse::<ServerNkey>(), Err(IdError::ServerNkey));
        assert!(user.parse::<UserNkey>().is_ok());
    }

    #[test]
    fn registry_rejects_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let a: TenantMapping = "acme=APP_A".parse()?;
        assert!(TenantRegistry::new([a.clone(), a]).is_err());
        let registry = TenantRegistry::new(["acme=APP_A".parse::<TenantMapping>()?])?;
        assert_eq!(
            registry.account(&"acme".parse()?).map(AccountName::as_str),
            Some("APP_A")
        );
        Ok(())
    }

    #[test]
    fn counters_advance_without_wrapping() {
        assert_eq!(AuthEpoch::INITIAL.next(), Some(AuthEpoch::new(2)));
        assert_eq!(AuthVersion::new(u64::MAX).next(), None);
    }
}
