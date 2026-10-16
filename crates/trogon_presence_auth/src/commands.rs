use std::time::Duration;

use trogon_presence::{ConnectionId, Expected, OperationId, Topic};

use crate::claims::{ConnectIdentity, Subject, UnixSeconds};
use crate::grants::PrivateTopicCap;
use crate::ids::{AuthEpoch, AuthRealmId, AuthSessionId, AuthVersion, TenantId};
use crate::rows::{
    AuthKey, CommandKind, CommandReceipt, ConnectionRow, Enrolled, PolicyRow, RowStatus, SessionRow, SweepRow,
    SweepScope, SweepStatus, AUTH_ROW_SCHEMA_V1,
};
use crate::store::{AuthStore, Row, StoreError, Write, WriteOutcome};

pub const RECEIPT_TTL: Duration = Duration::from_secs(300);
pub const ROW_GRACE: Duration = Duration::from_secs(60);
pub const DEFAULT_SESSION_CEILING: Duration = Duration::from_secs(24 * 60 * 60);
const COMMAND_ATTEMPTS: usize = 5;

/// Longest application-session lifetime a refresh may grant, measured from the refresh time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCeiling(Duration);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("session ceiling must be a positive whole number of seconds")]
pub struct SessionCeilingError;

impl SessionCeiling {
    pub fn new(ceiling: Duration) -> Result<Self, SessionCeilingError> {
        if ceiling.is_zero() || ceiling.subsec_nanos() != 0 {
            return Err(SessionCeilingError);
        }
        Ok(Self(ceiling))
    }

    pub fn get(self) -> Duration {
        self.0
    }

    /// Requested expiry clamped to `now + ceiling`, never shortening the current expiry.
    pub fn clamp(self, now: UnixSeconds, current: UnixSeconds, requested: UnixSeconds) -> UnixSeconds {
        requested.min(now.saturating_add(self.0)).max(current)
    }
}

impl Default for SessionCeiling {
    fn default() -> Self {
        Self(DEFAULT_SESSION_CEILING)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTarget {
    pub operation: OperationId,
    pub tenant: TenantId,
    pub sub: Subject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTarget {
    pub user: UserTarget,
    pub sid: AuthSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollRequest {
    pub user: UserTarget,
    pub realm: AuthRealmId,
    pub sid: AuthSessionId,
    pub cid: ConnectionId,
    pub topics: Vec<Topic>,
    pub session_expires_at: UnixSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshRequest {
    pub session: SessionTarget,
    pub realm: AuthRealmId,
    pub cid: ConnectionId,
    pub session_expires_at: UnixSeconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandState {
    Committed,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub receipt: CommandReceipt,
    pub state: CommandState,
}

impl CommandOutcome {
    pub fn state_committed(&self) -> bool {
        self.state == CommandState::Committed
    }

    pub fn sweep(&self) -> &SweepStatus {
        &self.receipt.sweep
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollOutcome {
    pub outcome: CommandOutcome,
    pub identity: ConnectIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("user is not enrolled")]
    UnknownUser,
    #[error("session is not enrolled")]
    UnknownSession,
    #[error("user is disabled")]
    UserDisabled,
    #[error("user belongs to another realm")]
    RealmMismatch,
    #[error("session is revoked")]
    SessionRevoked,
    #[error("session lifetime is exhausted")]
    SessionExpired,
    #[error("connection is not enrolled")]
    UnknownConnection,
    #[error("connection belongs to a revoked user epoch; enroll again")]
    ConnectionRevoked,
    #[error("{count} topics exceed the cap of {cap}")]
    TooManyTopics { count: usize, cap: usize },
    #[error("counter overflow")]
    CounterExhausted,
    #[error("operation id was already used by a {0:?} command")]
    OperationReused(CommandKind),
    #[error("policy kept changing; retry the command")]
    Contended,
    #[error("store rejected the command: {0}")]
    Rejected(String),
}

#[derive(Debug, Clone)]
pub struct AuthCommands {
    store: AuthStore,
    topic_cap: PrivateTopicCap,
    session_ceiling: SessionCeiling,
}

struct Plan {
    writes: Vec<Write>,
    receipt: CommandReceipt,
}

impl AuthCommands {
    pub fn new(store: AuthStore, topic_cap: PrivateTopicCap) -> Self {
        Self {
            store,
            topic_cap,
            session_ceiling: SessionCeiling::default(),
        }
    }

    pub fn with_session_ceiling(mut self, ceiling: SessionCeiling) -> Self {
        self.session_ceiling = ceiling;
        self
    }

    pub fn store(&self) -> &AuthStore {
        &self.store
    }

    pub async fn enroll(&self, request: &EnrollRequest) -> Result<EnrollOutcome, CommandError> {
        let distinct: std::collections::BTreeSet<_> = request.topics.iter().map(Topic::tokens).collect();
        if distinct.len() > self.topic_cap.get() {
            return Err(CommandError::TooManyTopics {
                count: distinct.len(),
                cap: self.topic_cap.get(),
            });
        }
        let outcome = self
            .run(&request.user, CommandKind::Enroll, |now| self.plan_enroll(request, now))
            .await?;
        Self::identified(outcome, &request.user, &request.realm)
    }

    /// Re-issues the connection under the same session: bumps its AuthVersion so tokens minted
    /// before the refresh fail the callout version check, and extends the session expiry up to the
    /// configured ceiling.
    pub async fn refresh_session(&self, request: &RefreshRequest) -> Result<EnrollOutcome, CommandError> {
        let outcome = self
            .run(&request.session.user, CommandKind::RefreshSession, |now| {
                self.plan_refresh(request, now)
            })
            .await?;
        Self::identified(outcome, &request.session.user, &request.realm)
    }

    fn identified(
        outcome: CommandOutcome,
        user: &UserTarget,
        realm: &AuthRealmId,
    ) -> Result<EnrollOutcome, CommandError> {
        let enrolled = outcome
            .receipt
            .enrolled
            .clone()
            .ok_or(CommandError::OperationReused(outcome.receipt.command))?;
        let identity = ConnectIdentity {
            sub: user.sub.clone(),
            tenant: user.tenant.clone(),
            sid: enrolled.sid,
            cid: enrolled.cid,
            auth_realm: realm.clone(),
            auth_epoch: outcome.receipt.epoch,
            asv: enrolled.version,
            session_expires_at: enrolled.session_expires_at,
        };
        Ok(EnrollOutcome { outcome, identity })
    }

    pub async fn revoke_session(&self, target: &SessionTarget) -> Result<CommandOutcome, CommandError> {
        self.run(&target.user, CommandKind::RevokeSession, |now| {
            self.plan_revoke_session(target, now)
        })
        .await
    }

    pub async fn revoke_user(&self, target: &UserTarget) -> Result<CommandOutcome, CommandError> {
        self.run(target, CommandKind::RevokeUser, |now| {
            self.plan_user_bump(target, CommandKind::RevokeUser, now)
        })
        .await
    }

    pub async fn disable_user(&self, target: &UserTarget) -> Result<CommandOutcome, CommandError> {
        self.run(target, CommandKind::DisableUser, |now| {
            self.plan_user_bump(target, CommandKind::DisableUser, now)
        })
        .await
    }

    async fn run<F, Fut>(&self, user: &UserTarget, kind: CommandKind, plan: F) -> Result<CommandOutcome, CommandError>
    where
        F: Fn(UnixSeconds) -> Fut,
        Fut: std::future::Future<Output = Result<Plan, CommandError>>,
    {
        let receipt_key = AuthKey::receipt(&user.tenant, &user.sub, &user.operation);
        for _ in 0..COMMAND_ATTEMPTS {
            if let Some((_, receipt)) = self.store.read::<CommandReceipt>(&receipt_key).await?.present() {
                if receipt.command != kind {
                    return Err(CommandError::OperationReused(receipt.command));
                }
                return Ok(CommandOutcome {
                    receipt,
                    state: CommandState::Replayed,
                });
            }
            let Plan { writes, receipt } = plan(UnixSeconds::now()).await?;
            match self.store.commit(writes).await? {
                WriteOutcome::Committed => {
                    return Ok(CommandOutcome {
                        receipt,
                        state: CommandState::Committed,
                    })
                }
                WriteOutcome::Conflict | WriteOutcome::Unknown => {
                    if let Some((_, stored)) = self.store.read::<CommandReceipt>(&receipt_key).await?.present() {
                        if stored == receipt {
                            return Ok(CommandOutcome {
                                receipt,
                                state: CommandState::Committed,
                            });
                        }
                    }
                }
                WriteOutcome::Rejected(reason) => return Err(CommandError::Rejected(reason)),
            }
        }
        Err(CommandError::Contended)
    }

    fn finish(
        user: &UserTarget,
        kind: CommandKind,
        epoch: AuthEpoch,
        enrolled: Option<Enrolled>,
        scope: Option<SweepScope>,
        now: UnixSeconds,
        mut writes: Vec<Write>,
    ) -> Result<Plan, CommandError> {
        let sweep = match scope {
            Some(scope) => {
                let row = SweepRow {
                    v: AUTH_ROW_SCHEMA_V1,
                    operation: user.operation,
                    tenant: user.tenant.clone(),
                    sub: user.sub.clone(),
                    scope: scope.clone(),
                    epoch,
                    created_at: now,
                    completed_at: None,
                };
                writes.push(Write::put(
                    AuthKey::sweep(&user.tenant, &user.sub, &user.operation),
                    &row,
                    Expected::Empty,
                )?);
                SweepStatus::Pending {
                    sweep: user.operation,
                    scope,
                }
            }
            None => SweepStatus::NotRequired,
        };
        let receipt = CommandReceipt {
            v: AUTH_ROW_SCHEMA_V1,
            operation: user.operation,
            command: kind,
            epoch,
            enrolled,
            sweep,
            committed_at: now,
        };
        writes.push(
            Write::put(
                AuthKey::receipt(&user.tenant, &user.sub, &user.operation),
                &receipt,
                Expected::Empty,
            )?
            .expiring(RECEIPT_TTL)?,
        );
        Ok(Plan { writes, receipt })
    }

    async fn plan_enroll(&self, request: &EnrollRequest, now: UnixSeconds) -> Result<Plan, CommandError> {
        let user = &request.user;
        let policy_key = AuthKey::policy(&user.tenant, &user.sub);
        let policy_row = self.store.read::<PolicyRow>(&policy_key).await?;
        let policy_expected = policy_row.expected();
        let policy = match policy_row.present() {
            Some((_, policy)) if policy.status == RowStatus::Disabled => return Err(CommandError::UserDisabled),
            Some((_, policy)) if policy.realm != request.realm => return Err(CommandError::RealmMismatch),
            Some((_, policy)) => policy,
            None => PolicyRow::new(request.realm.clone()),
        };
        let mut writes = vec![Write::put(policy_key, &policy, policy_expected)?];

        let session_key = AuthKey::session(&user.tenant, &user.sub, &request.sid);
        let session_row = self.store.read::<SessionRow>(&session_key).await?;
        let session_expected = session_row.expected();
        let session_expires_at = match session_row.present() {
            Some((_, session)) if session.status == RowStatus::Disabled => return Err(CommandError::SessionRevoked),
            Some((_, session)) => session.expires_at,
            None => {
                let session = SessionRow {
                    v: AUTH_ROW_SCHEMA_V1,
                    expires_at: request.session_expires_at,
                    status: RowStatus::Active,
                };
                writes.push(Write::put(session_key, &session, session_expected)?);
                request.session_expires_at
            }
        };
        if session_expires_at <= now {
            return Err(CommandError::SessionExpired);
        }

        let connection_key = AuthKey::connection(&user.tenant, &user.sub, &request.sid, &request.cid);
        let connection_row = self.store.read::<ConnectionRow>(&connection_key).await?;
        let connection_expected = connection_row.expected();
        let (version, scope) = match connection_row.present() {
            Some((_, previous)) => {
                let version = previous.version.next().ok_or(CommandError::CounterExhausted)?;
                let scope = SweepScope::Connection {
                    sid: request.sid.clone(),
                    cid: request.cid,
                    below: version,
                };
                (version, Some(scope))
            }
            None => (AuthVersion::INITIAL, None),
        };
        let connection = ConnectionRow {
            v: AUTH_ROW_SCHEMA_V1,
            epoch: policy.epoch,
            version,
            topics: request.topics.clone(),
            expires_at: session_expires_at,
        };
        let ttl = now.until(session_expires_at) + ROW_GRACE;
        writes.push(Write::put(connection_key, &connection, connection_expected)?.expiring(ttl)?);

        let enrolled = Enrolled {
            sid: request.sid.clone(),
            cid: request.cid,
            version,
            session_expires_at,
        };
        Self::finish(
            user,
            CommandKind::Enroll,
            policy.epoch,
            Some(enrolled),
            scope,
            now,
            writes,
        )
    }

    async fn plan_refresh(&self, request: &RefreshRequest, now: UnixSeconds) -> Result<Plan, CommandError> {
        let user = &request.session.user;
        let sid = &request.session.sid;
        let (policy_key, policy, policy_expected) = self.current_policy(user).await?;
        if policy.realm != request.realm {
            return Err(CommandError::RealmMismatch);
        }
        let session_key = AuthKey::session(&user.tenant, &user.sub, sid);
        let session_row = self.store.read::<SessionRow>(&session_key).await?;
        let session_expected = session_row.expected();
        let (_, session) = session_row.present().ok_or(CommandError::UnknownSession)?;
        let connection_key = AuthKey::connection(&user.tenant, &user.sub, sid, &request.cid);
        let connection_row = self.store.read::<ConnectionRow>(&connection_key).await?;
        let connection_expected = connection_row.expected();
        let (_, connection) = connection_row.present().ok_or(CommandError::UnknownConnection)?;

        let refreshed = refresh_rows(
            &policy,
            session,
            connection,
            request.session_expires_at,
            self.session_ceiling,
            now,
        )?;
        let version = refreshed.connection.version;
        let session_expires_at = refreshed.session.expires_at;
        let ttl = now.until(session_expires_at) + ROW_GRACE;
        let writes = vec![
            Write::put(policy_key, &policy, policy_expected)?,
            Write::put(session_key, &refreshed.session, session_expected)?,
            Write::put(connection_key, &refreshed.connection, connection_expected)?.expiring(ttl)?,
        ];
        let enrolled = Enrolled {
            sid: sid.clone(),
            cid: request.cid,
            version,
            session_expires_at,
        };
        let scope = SweepScope::Connection {
            sid: sid.clone(),
            cid: request.cid,
            below: version,
        };
        Self::finish(
            user,
            CommandKind::RefreshSession,
            policy.epoch,
            Some(enrolled),
            Some(scope),
            now,
            writes,
        )
    }

    async fn plan_revoke_session(&self, target: &SessionTarget, now: UnixSeconds) -> Result<Plan, CommandError> {
        let user = &target.user;
        let (policy_key, mut policy, policy_expected) = self.current_policy(user).await?;
        policy.epoch = policy.epoch.next().ok_or(CommandError::CounterExhausted)?;
        let session_key = AuthKey::session(&user.tenant, &user.sub, &target.sid);
        let session_row = self.store.read::<SessionRow>(&session_key).await?;
        let session_expected = session_row.expected();
        let (_, mut session) = session_row.present().ok_or(CommandError::UnknownSession)?;
        session.status = RowStatus::Disabled;
        let writes = vec![
            Write::put(policy_key, &policy, policy_expected)?,
            Write::put(session_key, &session, session_expected)?,
        ];
        let scope = SweepScope::Session {
            sid: target.sid.clone(),
        };
        Self::finish(
            user,
            CommandKind::RevokeSession,
            policy.epoch,
            None,
            Some(scope),
            now,
            writes,
        )
    }

    async fn plan_user_bump(
        &self,
        user: &UserTarget,
        kind: CommandKind,
        now: UnixSeconds,
    ) -> Result<Plan, CommandError> {
        let (policy_key, mut policy, policy_expected) = self.current_policy(user).await?;
        policy.epoch = policy.epoch.next().ok_or(CommandError::CounterExhausted)?;
        if kind == CommandKind::DisableUser {
            policy.status = RowStatus::Disabled;
        }
        let writes = vec![Write::put(policy_key, &policy, policy_expected)?];
        Self::finish(user, kind, policy.epoch, None, Some(SweepScope::User), now, writes)
    }

    async fn current_policy(&self, user: &UserTarget) -> Result<(AuthKey, PolicyRow, Expected), CommandError> {
        let key = AuthKey::policy(&user.tenant, &user.sub);
        let row = self.store.read::<PolicyRow>(&key).await?;
        let expected = row.expected();
        let (_, policy) = row.present().ok_or(CommandError::UnknownUser)?;
        Ok((key, policy, expected))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Refreshed {
    session: SessionRow,
    connection: ConnectionRow,
}

fn refresh_rows(
    policy: &PolicyRow,
    mut session: SessionRow,
    mut connection: ConnectionRow,
    requested: UnixSeconds,
    ceiling: SessionCeiling,
    now: UnixSeconds,
) -> Result<Refreshed, CommandError> {
    if policy.status == RowStatus::Disabled {
        return Err(CommandError::UserDisabled);
    }
    if session.status == RowStatus::Disabled {
        return Err(CommandError::SessionRevoked);
    }
    if session.expires_at <= now {
        return Err(CommandError::SessionExpired);
    }
    if connection.epoch != policy.epoch {
        return Err(CommandError::ConnectionRevoked);
    }
    let expires_at = ceiling.clamp(now, session.expires_at, requested);
    session.expires_at = expires_at;
    connection.version = connection.version.next().ok_or(CommandError::CounterExhausted)?;
    connection.expires_at = expires_at;
    Ok(Refreshed { session, connection })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionView {
    pub policy: PolicyRow,
    pub session: SessionRow,
    pub connection: ConnectionRow,
    policy_revision: trogon_presence::EntryRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingRow {
    Policy,
    Session,
    Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionRead {
    Consistent(AdmissionView),
    Changed,
    Missing(MissingRow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitOutcome {
    Admitted,
    PolicyMoved,
    GrantConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionKeys {
    pub policy: AuthKey,
    pub session: AuthKey,
    pub connection: AuthKey,
}

impl AdmissionKeys {
    pub fn of(identity: &ConnectIdentity) -> Self {
        Self {
            policy: AuthKey::policy(&identity.tenant, &identity.sub),
            session: AuthKey::session(&identity.tenant, &identity.sub, &identity.sid),
            connection: AuthKey::connection(&identity.tenant, &identity.sub, &identity.sid, &identity.cid),
        }
    }
}

impl AuthStore {
    pub async fn read_admission(&self, keys: &AdmissionKeys) -> Result<AdmissionRead, StoreError> {
        let Some((first, policy)) = self.read::<PolicyRow>(&keys.policy).await?.present() else {
            return Ok(AdmissionRead::Missing(MissingRow::Policy));
        };
        let Some((_, session)) = self.read::<SessionRow>(&keys.session).await?.present() else {
            return Ok(AdmissionRead::Missing(MissingRow::Session));
        };
        let Some((_, connection)) = self.read::<ConnectionRow>(&keys.connection).await?.present() else {
            return Ok(AdmissionRead::Missing(MissingRow::Connection));
        };
        let again = self.read::<PolicyRow>(&keys.policy).await?;
        match again {
            Row::Present(second, ref reread) if second == first && *reread == policy => {
                Ok(AdmissionRead::Consistent(AdmissionView {
                    policy,
                    session,
                    connection,
                    policy_revision: first,
                }))
            }
            Row::Present(..) | Row::Removed(_) | Row::Missing => Ok(AdmissionRead::Changed),
        }
    }

    pub async fn admit<T>(
        &self,
        view: &AdmissionView,
        keys: &AdmissionKeys,
        grant_key: &AuthKey,
        grant: &T,
        ttl: Duration,
    ) -> Result<AdmitOutcome, StoreError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq,
    {
        let existing = self.read::<T>(grant_key).await?;
        let grant_expected = existing.expected();
        if let Some((_, stored)) = existing.present() {
            return Ok(if stored == *grant {
                AdmitOutcome::Admitted
            } else {
                AdmitOutcome::GrantConflict
            });
        }
        let writes = vec![
            Write::put(keys.policy.clone(), &view.policy, Expected::At(view.policy_revision))?,
            Write::put(grant_key.clone(), grant, grant_expected)?.expiring(ttl)?,
        ];
        match self.commit(writes).await? {
            WriteOutcome::Committed => Ok(AdmitOutcome::Admitted),
            WriteOutcome::Conflict | WriteOutcome::Unknown => match self.read::<T>(grant_key).await?.present() {
                Some((_, stored)) if stored == *grant => Ok(AdmitOutcome::Admitted),
                Some(_) => Ok(AdmitOutcome::GrantConflict),
                None => Ok(AdmitOutcome::PolicyMoved),
            },
            WriteOutcome::Rejected(_) => Ok(AdmitOutcome::PolicyMoved),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const NOW: UnixSeconds = UnixSeconds::new(1_000_000);

    fn rows() -> Result<(PolicyRow, SessionRow, ConnectionRow), Box<dyn std::error::Error>> {
        let policy = PolicyRow::new("realm".parse()?);
        let session = SessionRow {
            v: AUTH_ROW_SCHEMA_V1,
            expires_at: NOW.saturating_add(Duration::from_secs(600)),
            status: RowStatus::Active,
        };
        let connection = ConnectionRow {
            v: AUTH_ROW_SCHEMA_V1,
            epoch: policy.epoch,
            version: AuthVersion::INITIAL,
            topics: vec!["rooms.alpha".parse()?],
            expires_at: session.expires_at,
        };
        Ok((policy, session, connection))
    }

    fn ceiling(secs: u64) -> Result<SessionCeiling, SessionCeilingError> {
        SessionCeiling::new(Duration::from_secs(secs))
    }

    #[test]
    fn refresh_bumps_version_and_extends_expiry() -> TestResult {
        let (policy, session, connection) = rows()?;
        let requested = NOW.saturating_add(Duration::from_secs(3600));
        let refreshed = refresh_rows(
            &policy,
            session.clone(),
            connection.clone(),
            requested,
            ceiling(7200)?,
            NOW,
        )?;
        assert_eq!(refreshed.connection.version, AuthVersion::new(2));
        assert_eq!(refreshed.connection.epoch, connection.epoch);
        assert_eq!(refreshed.connection.topics, connection.topics);
        assert_eq!(refreshed.connection.expires_at, requested);
        assert_eq!(refreshed.session.expires_at, requested);
        assert_eq!(refreshed.session.status, RowStatus::Active);
        Ok(())
    }

    #[test]
    fn refresh_clamps_to_the_ceiling_and_never_shortens() -> TestResult {
        let (policy, session, connection) = rows()?;
        let far = NOW.saturating_add(Duration::from_secs(100_000));
        let clamped = refresh_rows(&policy, session.clone(), connection.clone(), far, ceiling(7200)?, NOW)?;
        assert_eq!(
            clamped.session.expires_at,
            NOW.saturating_add(Duration::from_secs(7200))
        );

        let earlier = NOW.saturating_add(Duration::from_secs(10));
        let kept = refresh_rows(&policy, session.clone(), connection, earlier, ceiling(7200)?, NOW)?;
        assert_eq!(kept.session.expires_at, session.expires_at);
        Ok(())
    }

    #[test]
    fn refresh_refuses_revoked_expired_disabled_and_stale_rows() -> TestResult {
        let (policy, session, connection) = rows()?;
        let requested = NOW.saturating_add(Duration::from_secs(3600));
        let ceiling = ceiling(7200)?;

        let mut revoked = session.clone();
        revoked.status = RowStatus::Disabled;
        assert!(matches!(
            refresh_rows(&policy, revoked, connection.clone(), requested, ceiling, NOW),
            Err(CommandError::SessionRevoked)
        ));

        let mut expired = session.clone();
        expired.expires_at = NOW;
        assert!(matches!(
            refresh_rows(&policy, expired, connection.clone(), requested, ceiling, NOW),
            Err(CommandError::SessionExpired)
        ));

        let mut disabled = policy.clone();
        disabled.status = RowStatus::Disabled;
        assert!(matches!(
            refresh_rows(&disabled, session.clone(), connection.clone(), requested, ceiling, NOW),
            Err(CommandError::UserDisabled)
        ));

        let mut bumped = policy.clone();
        bumped.epoch = AuthEpoch::new(2);
        assert!(matches!(
            refresh_rows(&bumped, session.clone(), connection.clone(), requested, ceiling, NOW),
            Err(CommandError::ConnectionRevoked)
        ));

        let mut exhausted = connection;
        exhausted.version = AuthVersion::new(u64::MAX);
        assert!(matches!(
            refresh_rows(&policy, session, exhausted, requested, ceiling, NOW),
            Err(CommandError::CounterExhausted)
        ));
        Ok(())
    }

    #[test]
    fn session_ceiling_rejects_zero_and_fractions() {
        assert!(SessionCeiling::new(Duration::ZERO).is_err());
        assert!(SessionCeiling::new(Duration::from_millis(1500)).is_err());
        assert!(SessionCeiling::new(Duration::from_secs(1)).is_ok());
    }
}
