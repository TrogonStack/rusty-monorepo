use std::fmt;

use serde::{Deserialize, Serialize};
use trogon_presence::{ConnectionId, OperationId, Topic};

use crate::claims::{AccountName, Subject, UnixSeconds};
use crate::ids::{AuthEpoch, AuthRealmId, AuthSessionId, AuthVersion, BrokerClientId, ServerNkey, TenantId};

pub const AUTH_ROW_SCHEMA_V1: u8 = 1;
const POLICY_SEGMENT: &str = "policy";
const SESSION_SEGMENT: &str = "session";
const CONNECTION_SEGMENT: &str = "connection";
const RECEIPT_SEGMENT: &str = "receipt";
pub(crate) const SWEEP_SEGMENT: &str = "sweep";
pub(crate) const GRANT_SEGMENT: &str = "grant";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AuthKey(String);

impl AuthKey {
    pub fn policy(tenant: &TenantId, sub: &Subject) -> Self {
        Self(format!("{POLICY_SEGMENT}.{}.{}", tenant.token(), sub.token()))
    }

    pub fn session(tenant: &TenantId, sub: &Subject, sid: &AuthSessionId) -> Self {
        Self(format!(
            "{SESSION_SEGMENT}.{}.{}.{}",
            tenant.token(),
            sub.token(),
            sid.token()
        ))
    }

    pub fn connection(tenant: &TenantId, sub: &Subject, sid: &AuthSessionId, cid: &ConnectionId) -> Self {
        Self(format!(
            "{CONNECTION_SEGMENT}.{}.{}.{}.{cid}",
            tenant.token(),
            sub.token(),
            sid.token()
        ))
    }

    pub fn receipt(tenant: &TenantId, sub: &Subject, operation: &OperationId) -> Self {
        Self(format!(
            "{RECEIPT_SEGMENT}.{}.{}.{operation}",
            tenant.token(),
            sub.token()
        ))
    }

    pub fn sweep(tenant: &TenantId, sub: &Subject, operation: &OperationId) -> Self {
        Self(format!(
            "{SWEEP_SEGMENT}.{}.{}.{operation}",
            tenant.token(),
            sub.token()
        ))
    }

    pub fn grant(server: &ServerNkey, client: BrokerClientId) -> Self {
        Self(format!("{GRANT_SEGMENT}.{server}.{client}"))
    }

    /// Parses a stored `grant.<server-NKey>.<server-CID>` key back into its broker connection.
    pub fn parse_grant(key: &str) -> Option<(ServerNkey, BrokerClientId)> {
        let rest = key.strip_prefix(GRANT_SEGMENT)?.strip_prefix('.')?;
        let (server, client) = rest.split_once('.')?;
        let server = server.parse().ok()?;
        let client = client.parse::<u64>().ok()?;
        Some((server, BrokerClientId::new(client)))
    }

    pub fn from_stored(key: &str) -> Self {
        Self(key.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    Active,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRow {
    pub v: u8,
    pub realm: AuthRealmId,
    pub epoch: AuthEpoch,
    pub status: RowStatus,
}

impl PolicyRow {
    pub fn new(realm: AuthRealmId) -> Self {
        Self {
            v: AUTH_ROW_SCHEMA_V1,
            realm,
            epoch: AuthEpoch::INITIAL,
            status: RowStatus::Active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub v: u8,
    pub expires_at: UnixSeconds,
    pub status: RowStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRow {
    pub v: u8,
    pub epoch: AuthEpoch,
    pub version: AuthVersion,
    pub topics: Vec<Topic>,
    pub expires_at: UnixSeconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Enroll,
    RevokeSession,
    RevokeUser,
    DisableUser,
    RefreshSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SweepScope {
    User,
    Session {
        sid: AuthSessionId,
    },
    Connection {
        sid: AuthSessionId,
        cid: ConnectionId,
        below: AuthVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SweepStatus {
    NotRequired,
    Pending { sweep: OperationId, scope: SweepScope },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolled {
    pub sid: AuthSessionId,
    pub cid: ConnectionId,
    pub version: AuthVersion,
    pub session_expires_at: UnixSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub v: u8,
    pub operation: OperationId,
    pub command: CommandKind,
    pub epoch: AuthEpoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled: Option<Enrolled>,
    pub sweep: SweepStatus,
    pub committed_at: UnixSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepRow {
    pub v: u8,
    pub operation: OperationId,
    pub tenant: TenantId,
    pub sub: Subject,
    pub scope: SweepScope,
    pub epoch: AuthEpoch,
    pub created_at: UnixSeconds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixSeconds>,
}

impl SweepRow {
    pub fn key(&self) -> AuthKey {
        AuthKey::sweep(&self.tenant, &self.sub, &self.operation)
    }

    pub fn is_complete(&self) -> bool {
        self.completed_at.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRow {
    pub v: u8,
    pub tenant: TenantId,
    pub sub: Subject,
    pub account: AccountName,
    pub sid: AuthSessionId,
    pub cid: ConnectionId,
    pub epoch: AuthEpoch,
    pub version: AuthVersion,
    pub expires_at: UnixSeconds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_contract() -> Result<(), Box<dyn std::error::Error>> {
        let tenant: TenantId = "acme.io".parse()?;
        let sub = Subject::try_from("ana@x.io".to_owned())?;
        let sid: AuthSessionId = "s.1".parse()?;
        let cid = ConnectionId::generate()?;
        let op = OperationId::generate()?;
        assert_eq!(AuthKey::policy(&tenant, &sub).as_str(), "policy.acme=2Eio.ana=40x=2Eio");
        assert_eq!(
            AuthKey::session(&tenant, &sub, &sid).as_str(),
            "session.acme=2Eio.ana=40x=2Eio.s=2E1"
        );
        assert_eq!(
            AuthKey::connection(&tenant, &sub, &sid, &cid).as_str(),
            format!("connection.acme=2Eio.ana=40x=2Eio.s=2E1.{cid}")
        );
        assert_eq!(
            AuthKey::receipt(&tenant, &sub, &op).as_str(),
            format!("receipt.acme=2Eio.ana=40x=2Eio.{op}")
        );
        assert_eq!(
            AuthKey::sweep(&tenant, &sub, &op).as_str(),
            format!("sweep.acme=2Eio.ana=40x=2Eio.{op}")
        );
        let server: ServerNkey = nkeys::KeyPair::new_server().public_key().parse()?;
        let grant = AuthKey::grant(&server, BrokerClientId::new(42));
        assert_eq!(grant.as_str(), format!("grant.{server}.42"));
        assert_eq!(
            AuthKey::parse_grant(grant.as_str()),
            Some((server.clone(), BrokerClientId::new(42)))
        );
        assert_eq!(AuthKey::parse_grant(&format!("grant.{server}.x")), None);
        assert_eq!(AuthKey::parse_grant("grant.NOPE.1"), None);
        assert_eq!(AuthKey::parse_grant(&format!("sweep.{server}.1")), None);
        Ok(())
    }

    #[test]
    fn rows_round_trip_as_json() -> Result<(), Box<dyn std::error::Error>> {
        let policy = PolicyRow::new("realm".parse()?);
        assert_eq!(
            serde_json::to_string(&policy)?,
            r#"{"v":1,"realm":"realm","epoch":1,"status":"active"}"#
        );
        let status = SweepStatus::Pending {
            sweep: OperationId::generate()?,
            scope: SweepScope::User,
        };
        let back: SweepStatus = serde_json::from_slice(&serde_json::to_vec(&status)?)?;
        assert_eq!(back, status);
        Ok(())
    }
}
