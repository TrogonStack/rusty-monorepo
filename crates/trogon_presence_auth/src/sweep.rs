//! Durable revocation sweeps: resolves obsolete admission-ledger grants, cross-checks them against
//! per-server CONNZ and kicks them until a post-admission-window pass confirms they are gone.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use trogon_presence::watch::replay::{
    RawRecord, RebuildBudget, ReplayConsumer, ReplayError, ReplayFilters, ReplayFiltersError,
};
use trogon_presence::{Expected, OperationId};

use crate::callout::UserJwtLifetime;
use crate::claims::{AccountName, UnixSeconds};
use crate::commands::UserTarget;
use crate::ids::{BrokerClientId, ServerNkey, TenantRegistry};
use crate::rows::{AuthKey, GrantRow, SweepRow, SweepScope, GRANT_SEGMENT, SWEEP_SEGMENT};
use crate::store::{decode_row, AuthStore, Row, StoreError, Write, WriteOutcome};

pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(3);
pub const DEFAULT_ADMISSION_WINDOW: Duration = Duration::from_secs(3);
pub const DEFAULT_CONNZ_PAGE_LIMIT: usize = 1024;
pub const DEFAULT_SYSTEM_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
pub const DEFAULT_SWEEP_DEADLINE: Duration = Duration::from_secs(30);
pub const COMPLETED_SWEEP_TTL: Duration = Duration::from_secs(300);
const WALL_CLOCK_GRANULARITY: Duration = Duration::from_secs(1);

macro_rules! positive_duration {
    ($name:ident, $error:ident, $default:expr, $what:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Duration);

        #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
        #[error("{} must be greater than zero", $what)]
        pub struct $error;

        impl $name {
            pub fn get(self) -> Duration {
                self.0
            }
        }

        impl TryFrom<Duration> for $name {
            type Error = $error;

            fn try_from(value: Duration) -> Result<Self, Self::Error> {
                if value.is_zero() {
                    Err($error)
                } else {
                    Ok(Self(value))
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }
    };
}

positive_duration!(
    SweepInterval,
    SweepIntervalError,
    DEFAULT_SWEEP_INTERVAL,
    "sweep interval"
);
positive_duration!(
    AdmissionWindow,
    AdmissionWindowError,
    DEFAULT_ADMISSION_WINDOW,
    "admission window"
);
positive_duration!(
    SystemRequestTimeout,
    SystemRequestTimeoutError,
    DEFAULT_SYSTEM_REQUEST_TIMEOUT,
    "system request timeout"
);
positive_duration!(
    SweepDeadline,
    SweepDeadlineError,
    DEFAULT_SWEEP_DEADLINE,
    "sweep deadline"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnzPageLimit(NonZeroUsize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("CONNZ page limit must be greater than zero")]
pub struct ConnzPageLimitError;

impl ConnzPageLimit {
    pub fn get(self) -> usize {
        self.0.get()
    }
}

impl TryFrom<usize> for ConnzPageLimit {
    type Error = ConnzPageLimitError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        NonZeroUsize::new(value).map(Self).ok_or(ConnzPageLimitError)
    }
}

impl Default for ConnzPageLimit {
    fn default() -> Self {
        Self(NonZeroUsize::new(DEFAULT_CONNZ_PAGE_LIMIT).unwrap_or(NonZeroUsize::MIN))
    }
}

/// The configured, authoritative set of servers every sweep must cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRoster(Vec<ServerNkey>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the server roster needs at least one server")]
pub struct ServerRosterError;

impl ServerRoster {
    pub fn new(servers: impl IntoIterator<Item = ServerNkey>) -> Result<Self, ServerRosterError> {
        let mut seen = HashSet::new();
        let servers: Vec<ServerNkey> = servers
            .into_iter()
            .filter(|server| seen.insert(server.clone()))
            .collect();
        if servers.is_empty() {
            return Err(ServerRosterError);
        }
        Ok(Self(servers))
    }

    pub fn servers(&self) -> &[ServerNkey] {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct SweepConfig {
    pub roster: ServerRoster,
    pub tenants: TenantRegistry,
    pub user_jwt_lifetime: UserJwtLifetime,
    pub interval: SweepInterval,
    pub admission_window: AdmissionWindow,
    pub page_limit: ConnzPageLimit,
    pub request_timeout: SystemRequestTimeout,
    pub rebuild_budget: RebuildBudget,
}

impl SweepConfig {
    pub fn new(roster: ServerRoster, tenants: TenantRegistry) -> Self {
        Self {
            roster,
            tenants,
            user_jwt_lifetime: UserJwtLifetime::default(),
            interval: SweepInterval::default(),
            admission_window: AdmissionWindow::default(),
            page_limit: ConnzPageLimit::default(),
            request_timeout: SystemRequestTimeout::default(),
            rebuild_budget: RebuildBudget::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrokerConnection {
    pub server: ServerNkey,
    pub client: BrokerClientId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KickFailure {
    pub connection: BrokerConnection,
    pub reason: String,
}

/// Why a roster server did not count as covered in a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerGap {
    NoResponders,
    Timeout,
    Request(String),
    Malformed(String),
    Error { code: i64, description: String },
    WrongServer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    Covered { connections: usize, pages: usize },
    Missing(ServerGap),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCoverage {
    pub server: ServerNkey,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Obsolete ledger connections were still present and kicked this pass.
    Kicking { remaining: usize },
    /// Every server is covered, but the admission window has not closed yet.
    AwaitingWindow { verify_after: UnixSeconds },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncompleteReason {
    MissingServerResponse {
        servers: Vec<ServerNkey>,
    },
    /// Connections in scope have no ledger attribution; the JWT ceiling bounds them instead.
    UnattributedConnections {
        connections: Vec<BrokerConnection>,
        ceiling: UnixSeconds,
    },
    UnregisteredTenant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepState {
    Pending,
    InProgress(Progress),
    Incomplete(Vec<IncompleteReason>),
    Complete { completed_at: UnixSeconds },
}

impl SweepState {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub operation: OperationId,
    pub state: SweepState,
    pub coverage: Vec<ServerCoverage>,
    pub kicked: Vec<BrokerConnection>,
    pub failed: Vec<KickFailure>,
}

#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("rebuilding the admission ledger: {0}")]
    Replay(#[from] ReplayError),
    #[error(transparent)]
    Filters(#[from] ReplayFiltersError),
    #[error("sweep {0} is not stored")]
    UnknownSweep(OperationId),
    #[error("sweep row kept changing while marking it complete")]
    Contended,
    #[error("store rejected the sweep completion: {0}")]
    Rejected(String),
}

#[derive(Debug, Clone, Copy)]
enum LedgerPart {
    Grants,
    Sweeps,
}

impl LedgerPart {
    fn segment(self) -> &'static str {
        match self {
            Self::Grants => GRANT_SEGMENT,
            Self::Sweeps => SWEEP_SEGMENT,
        }
    }
}

/// The admission ledger and sweep rows as rebuilt from the AUTH stream.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    grants: HashMap<BrokerConnection, GrantRow>,
    sweeps: HashMap<OperationId, SweepRow>,
    unreadable: usize,
}

impl Ledger {
    pub fn grant(&self, connection: &BrokerConnection) -> Option<&GrantRow> {
        self.grants.get(connection)
    }

    pub fn grants(&self) -> impl Iterator<Item = (&BrokerConnection, &GrantRow)> {
        self.grants.iter()
    }

    pub fn sweep(&self, operation: &OperationId) -> Option<&SweepRow> {
        self.sweeps.get(operation)
    }

    pub fn incomplete_sweeps(&self) -> impl Iterator<Item = &SweepRow> {
        self.sweeps.values().filter(|sweep| !sweep.is_complete())
    }

    pub fn unreadable(&self) -> usize {
        self.unreadable
    }

    fn apply(&mut self, store: &AuthStore, record: &RawRecord) {
        let Some(key) = store.bucket().key_of(record.subject()) else {
            self.unreadable += 1;
            return;
        };
        let stored = AuthKey::from_stored(key);
        if let Some((server, client)) = AuthKey::parse_grant(key) {
            let connection = BrokerConnection { server, client };
            match decode_row::<GrantRow>(&stored, record.headers(), record.payload()) {
                Ok(Some(grant)) => {
                    self.grants.insert(connection, grant);
                }
                Ok(None) => {
                    self.grants.remove(&connection);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "skipping an unreadable grant row");
                    self.unreadable += 1;
                }
            }
            return;
        }
        if key.starts_with(SWEEP_SEGMENT) {
            match decode_row::<SweepRow>(&stored, record.headers(), record.payload()) {
                Ok(Some(sweep)) if sweep.key() == stored => {
                    self.sweeps.insert(sweep.operation, sweep);
                }
                Ok(Some(_)) => {
                    tracing::warn!(key, "skipping a sweep row stored under another key");
                    self.unreadable += 1;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(error = %err, "skipping an unreadable sweep row");
                    self.unreadable += 1;
                }
            }
            return;
        }
        self.unreadable += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantFit {
    Outside,
    Current,
    Obsolete,
}

fn fit(sweep: &SweepRow, grant: &GrantRow) -> GrantFit {
    if grant.tenant != sweep.tenant || grant.sub != sweep.sub {
        return GrantFit::Outside;
    }
    let obsolete = match &sweep.scope {
        SweepScope::User => grant.epoch < sweep.epoch,
        SweepScope::Session { sid } if *sid == grant.sid => grant.epoch < sweep.epoch,
        SweepScope::Connection { sid, cid, below } if *sid == grant.sid && *cid == grant.cid => grant.version < *below,
        SweepScope::Session { .. } | SweepScope::Connection { .. } => return GrantFit::Outside,
    };
    if obsolete {
        GrantFit::Obsolete
    } else {
        GrantFit::Current
    }
}

#[derive(Debug, Serialize)]
struct ConnzRequest<'a> {
    acc: &'a str,
    user: &'a str,
    auth: bool,
    offset: usize,
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct ApiReply<T> {
    #[serde(default = "absent")]
    data: Option<T>,
    #[serde(default)]
    error: Option<ApiError>,
    server: ReplyServer,
}

fn absent<T>() -> Option<T> {
    None
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct ReplyServer {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ConnzPage {
    #[serde(default)]
    total: usize,
    #[serde(default)]
    connections: Vec<ConnzConnection>,
}

#[derive(Debug, Deserialize)]
struct ConnzConnection {
    cid: BrokerClientId,
    #[serde(default)]
    account: Option<String>,
}

#[derive(Debug, Serialize)]
struct KickRequest {
    cid: BrokerClientId,
}

/// The per-server CONNZ answers gathered for one sweep before the ledger is rebuilt.
#[derive(Debug, Clone)]
struct Observation {
    coverage: Vec<ServerCoverage>,
    present: HashSet<BrokerConnection>,
    unregistered: bool,
}

#[derive(Debug, Clone)]
pub struct SweepExecutor {
    store: AuthStore,
    system: async_nats::Client,
    config: Arc<SweepConfig>,
    last: Arc<Mutex<HashMap<OperationId, SweepState>>>,
}

impl SweepExecutor {
    pub fn new(store: AuthStore, system: async_nats::Client, config: SweepConfig) -> Self {
        Self {
            store,
            system,
            config: Arc::new(config),
            last: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &SweepConfig {
        &self.config
    }

    /// Rebuilds the grant and sweep rows of this AUTH bucket with a scoped `LastPerSubject` pull.
    pub async fn ledger(&self) -> Result<Ledger, SweepError> {
        self.rebuild(&[LedgerPart::Grants, LedgerPart::Sweeps]).await
    }

    async fn rebuild(&self, parts: &[LedgerPart]) -> Result<Ledger, SweepError> {
        let bucket = self.store.bucket();
        let filters: Vec<String> = parts.iter().map(|part| bucket.segment_filter(part.segment())).collect();
        let filters = ReplayFilters::try_from(filters)?;
        let mut ledger = Ledger::default();
        let replay = ReplayConsumer::rebuild(self.store.stream(), &filters, self.config.rebuild_budget, |record| {
            ledger.apply(&self.store, &record)
        })
        .await?;
        replay.discard();
        Ok(ledger)
    }

    /// The latest known state of a sweep: complete once stored as such, otherwise the last pass.
    pub async fn status(&self, target: &UserTarget) -> Result<SweepState, SweepError> {
        let key = AuthKey::sweep(&target.tenant, &target.sub, &target.operation);
        match self.store.read::<SweepRow>(&key).await? {
            Row::Present(
                _,
                SweepRow {
                    completed_at: Some(completed_at),
                    ..
                },
            ) => Ok(SweepState::Complete { completed_at }),
            Row::Present(..) => Ok(self.remembered(&target.operation).unwrap_or(SweepState::Pending)),
            Row::Missing | Row::Removed(_) => Err(SweepError::UnknownSweep(target.operation)),
        }
    }

    fn remembered(&self, operation: &OperationId) -> Option<SweepState> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(operation)
            .cloned()
    }

    fn remember(&self, report: &SweepReport) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if report.state.is_complete() {
            last.remove(&report.operation);
        } else {
            last.insert(report.operation, report.state.clone());
        }
    }

    /// Runs one pass over every incomplete sweep in the rebuilt ledger.
    pub async fn tick(&self) -> Result<Vec<SweepReport>, SweepError> {
        let sweeps: Vec<SweepRow> = self
            .rebuild(&[LedgerPart::Sweeps])
            .await?
            .incomplete_sweeps()
            .cloned()
            .collect();
        self.passes(&sweeps).await
    }

    /// Runs one pass for a single stored sweep.
    pub async fn pass(&self, target: &UserTarget) -> Result<SweepReport, SweepError> {
        let key = AuthKey::sweep(&target.tenant, &target.sub, &target.operation);
        let (_, sweep) = self
            .store
            .read::<SweepRow>(&key)
            .await?
            .present()
            .ok_or(SweepError::UnknownSweep(target.operation))?;
        let mut reports = self.passes(std::slice::from_ref(&sweep)).await?;
        reports.pop().ok_or(SweepError::UnknownSweep(target.operation))
    }

    /// Repeats passes at the configured interval until the sweep completes or the deadline passes.
    pub async fn drive(&self, target: &UserTarget, deadline: SweepDeadline) -> Result<SweepReport, SweepError> {
        let until = tokio::time::Instant::now() + deadline.get();
        let mut kicked = Vec::new();
        let mut failed = Vec::new();
        loop {
            let mut report = self.pass(target).await?;
            kicked.append(&mut report.kicked);
            failed.append(&mut report.failed);
            let next = tokio::time::Instant::now() + self.config.interval.get();
            if report.state.is_complete() || next > until {
                report.kicked = kicked;
                report.failed = failed;
                return Ok(report);
            }
            tokio::time::sleep_until(next).await;
        }
    }

    /// Resumes every incomplete sweep from the store and keeps sweeping at the configured interval.
    pub async fn serve(&self) {
        loop {
            match self.tick().await {
                Ok(reports) => {
                    for report in reports {
                        tracing::info!(
                            operation = %report.operation,
                            state = ?report.state,
                            kicked = report.kicked.len(),
                            failed = report.failed.len(),
                            "sweep pass"
                        );
                    }
                }
                Err(err) => tracing::warn!(error = %err, "sweep tick failed; retrying"),
            }
            tokio::time::sleep(self.config.interval.get()).await;
        }
    }

    async fn passes(&self, sweeps: &[SweepRow]) -> Result<Vec<SweepReport>, SweepError> {
        let mut observed = Vec::new();
        let mut reports = Vec::new();
        for sweep in sweeps {
            match sweep.completed_at {
                Some(completed_at) => reports.push(SweepReport {
                    operation: sweep.operation,
                    state: SweepState::Complete { completed_at },
                    coverage: Vec::new(),
                    kicked: Vec::new(),
                    failed: Vec::new(),
                }),
                None => {
                    let observation = self.observe(sweep).await;
                    observed.push((sweep, observation));
                }
            }
        }
        if !observed.is_empty() {
            let grants = self.rebuild(&[LedgerPart::Grants]).await?;
            for (sweep, observation) in observed {
                reports.push(self.settle(sweep, observation, &grants).await?);
            }
        }
        for report in &reports {
            self.remember(report);
        }
        Ok(reports)
    }

    async fn observe(&self, sweep: &SweepRow) -> Observation {
        let Some(account) = self.config.tenants.account(&sweep.tenant) else {
            return Observation {
                coverage: Vec::new(),
                present: HashSet::new(),
                unregistered: true,
            };
        };
        let name = sweep.sub.token();
        let mut coverage = Vec::new();
        let mut present = HashSet::new();
        for server in self.config.roster.servers() {
            let answered = match self.connz(server, account, name).await {
                Ok((clients, pages)) => {
                    let connections = clients.len();
                    present.extend(clients.into_iter().map(|client| BrokerConnection {
                        server: server.clone(),
                        client,
                    }));
                    Coverage::Covered { connections, pages }
                }
                Err(gap) => Coverage::Missing(gap),
            };
            coverage.push(ServerCoverage {
                server: server.clone(),
                coverage: answered,
            });
        }
        Observation {
            coverage,
            present,
            unregistered: false,
        }
    }

    async fn settle(
        &self,
        sweep: &SweepRow,
        observation: Observation,
        ledger: &Ledger,
    ) -> Result<SweepReport, SweepError> {
        let now = UnixSeconds::now();
        let mut report = SweepReport {
            operation: sweep.operation,
            state: SweepState::Pending,
            coverage: observation.coverage,
            kicked: Vec::new(),
            failed: Vec::new(),
        };
        if observation.unregistered {
            report.state = SweepState::Incomplete(vec![IncompleteReason::UnregisteredTenant]);
            return Ok(report);
        }
        let mut obsolete = Vec::new();
        let mut unattributed = Vec::new();
        for connection in &observation.present {
            match ledger.grant(connection).map(|grant| fit(sweep, grant)) {
                Some(GrantFit::Obsolete) => obsolete.push(connection.clone()),
                Some(GrantFit::Current | GrantFit::Outside) => {}
                None => unattributed.push(connection.clone()),
            }
        }
        for connection in &obsolete {
            match self.kick(connection).await {
                Ok(()) => report.kicked.push(connection.clone()),
                Err(reason) => report.failed.push(KickFailure {
                    connection: connection.clone(),
                    reason,
                }),
            }
        }
        let missing: Vec<ServerNkey> = report
            .coverage
            .iter()
            .filter(|server| matches!(server.coverage, Coverage::Missing(_)))
            .map(|server| server.server.clone())
            .collect();
        let mut reasons = Vec::new();
        if !missing.is_empty() {
            reasons.push(IncompleteReason::MissingServerResponse { servers: missing });
        }
        if !unattributed.is_empty() {
            reasons.push(IncompleteReason::UnattributedConnections {
                connections: unattributed,
                ceiling: self.ceiling(sweep, ledger),
            });
        }
        let verify_after = sweep
            .created_at
            .saturating_add(self.config.admission_window.get() + WALL_CLOCK_GRANULARITY);
        report.state = if !reasons.is_empty() {
            SweepState::Incomplete(reasons)
        } else if !obsolete.is_empty() {
            SweepState::InProgress(Progress::Kicking {
                remaining: obsolete.len(),
            })
        } else if now < verify_after {
            SweepState::InProgress(Progress::AwaitingWindow { verify_after })
        } else {
            SweepState::Complete {
                completed_at: self.complete(sweep, now).await?,
            }
        };
        Ok(report)
    }

    fn ceiling(&self, sweep: &SweepRow, ledger: &Ledger) -> UnixSeconds {
        let bound = sweep.created_at.saturating_add(
            self.config.user_jwt_lifetime.ceiling() + self.config.admission_window.get() + WALL_CLOCK_GRANULARITY,
        );
        ledger
            .grants()
            .filter(|(_, grant)| fit(sweep, grant) != GrantFit::Outside)
            .map(|(_, grant)| grant.expires_at)
            .fold(bound, UnixSeconds::max)
    }

    async fn complete(&self, sweep: &SweepRow, now: UnixSeconds) -> Result<UnixSeconds, SweepError> {
        let key = sweep.key();
        let (revision, stored) = match self.store.read::<SweepRow>(&key).await? {
            Row::Present(revision, stored) => (revision, stored),
            Row::Missing | Row::Removed(_) => return Err(SweepError::UnknownSweep(sweep.operation)),
        };
        if let Some(completed_at) = stored.completed_at {
            return Ok(completed_at);
        }
        let done = SweepRow {
            completed_at: Some(now),
            ..stored
        };
        let write = Write::put(key.clone(), &done, Expected::At(revision))?.expiring(COMPLETED_SWEEP_TTL)?;
        match self.store.commit(vec![write]).await? {
            WriteOutcome::Committed => Ok(now),
            WriteOutcome::Conflict | WriteOutcome::Unknown => match self.store.read::<SweepRow>(&key).await? {
                Row::Present(
                    _,
                    SweepRow {
                        completed_at: Some(completed_at),
                        ..
                    },
                ) => Ok(completed_at),
                Row::Present(..) | Row::Missing | Row::Removed(_) => Err(SweepError::Contended),
            },
            WriteOutcome::Rejected(reason) => Err(SweepError::Rejected(reason)),
        }
    }

    async fn system_request(&self, subject: String, body: Vec<u8>) -> Result<async_nats::Message, ServerGap> {
        let request = self.system.request(subject, body.into());
        match tokio::time::timeout(self.config.request_timeout.get(), request).await {
            Err(_) => Err(ServerGap::Timeout),
            Ok(Err(err)) if err.kind() == async_nats::RequestErrorKind::NoResponders => Err(ServerGap::NoResponders),
            Ok(Err(err)) if err.kind() == async_nats::RequestErrorKind::TimedOut => Err(ServerGap::Timeout),
            Ok(Err(err)) => Err(ServerGap::Request(err.to_string())),
            Ok(Ok(message)) => Ok(message),
        }
    }

    /// Pages the user's connections on one server. With a user filter the server still reports the
    /// account's unfiltered `total`, so a short page, not `total`, marks the last page.
    async fn connz(
        &self,
        server: &ServerNkey,
        account: &AccountName,
        name: &str,
    ) -> Result<(Vec<BrokerClientId>, usize), ServerGap> {
        let limit = self.config.page_limit.get();
        let mut clients = Vec::new();
        let mut pages = 0;
        let mut offset = 0;
        loop {
            let request = ConnzRequest {
                acc: account.as_str(),
                user: name,
                auth: true,
                offset,
                limit,
            };
            let body = serde_json::to_vec(&request).map_err(|err| ServerGap::Malformed(err.to_string()))?;
            let message = self
                .system_request(format!("$SYS.REQ.SERVER.{server}.CONNZ"), body)
                .await?;
            let page: ConnzPage = Self::answer(server, &message.payload)?;
            pages += 1;
            let received = page.connections.len();
            clients.extend(
                page.connections
                    .into_iter()
                    .filter(|connection| connection.account.as_deref().is_none_or(|acc| acc == account.as_str()))
                    .map(|connection| connection.cid),
            );
            offset += received;
            if received < limit || offset >= page.total {
                return Ok((clients, pages));
            }
        }
    }

    fn answer<T: serde::de::DeserializeOwned>(server: &ServerNkey, payload: &[u8]) -> Result<T, ServerGap> {
        let reply: ApiReply<T> =
            serde_json::from_slice(payload).map_err(|err| ServerGap::Malformed(err.to_string()))?;
        if reply.server.id != server.as_str() {
            return Err(ServerGap::WrongServer);
        }
        if let Some(error) = reply.error {
            return Err(ServerGap::Error {
                code: error.code,
                description: error.description,
            });
        }
        reply
            .data
            .ok_or_else(|| ServerGap::Malformed("reply carries no data".to_owned()))
    }

    async fn kick(&self, connection: &BrokerConnection) -> Result<(), String> {
        let body = serde_json::to_vec(&KickRequest { cid: connection.client }).map_err(|err| err.to_string())?;
        let message = self
            .system_request(format!("$SYS.REQ.SERVER.{}.KICK", connection.server), body)
            .await
            .map_err(|gap| format!("{gap:?}"))?;
        let reply: ApiReply<serde_json::Value> =
            serde_json::from_slice(&message.payload).map_err(|err| err.to_string())?;
        if reply.server.id != connection.server.as_str() {
            return Err("KICK answered by another server".to_owned());
        }
        match reply.error {
            Some(error) => Err(format!("{} {}", error.code, error.description)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AuthEpoch, AuthSessionId, AuthVersion};
    use crate::rows::AUTH_ROW_SCHEMA_V1;
    use trogon_presence::ConnectionId;

    fn grant(
        sid: &AuthSessionId,
        cid: ConnectionId,
        epoch: u64,
        version: u64,
    ) -> Result<GrantRow, Box<dyn std::error::Error>> {
        Ok(GrantRow {
            v: AUTH_ROW_SCHEMA_V1,
            tenant: "t".parse()?,
            sub: "ana".to_owned().try_into()?,
            account: "APP".parse()?,
            sid: sid.clone(),
            cid,
            epoch: AuthEpoch::new(epoch),
            version: AuthVersion::new(version),
            expires_at: UnixSeconds::new(100),
        })
    }

    fn sweep(scope: SweepScope, epoch: u64) -> Result<SweepRow, Box<dyn std::error::Error>> {
        Ok(SweepRow {
            v: AUTH_ROW_SCHEMA_V1,
            operation: OperationId::generate()?,
            tenant: "t".parse()?,
            sub: "ana".to_owned().try_into()?,
            scope,
            epoch: AuthEpoch::new(epoch),
            created_at: UnixSeconds::new(10),
            completed_at: None,
        })
    }

    #[test]
    fn current_grants_survive_an_older_sweep() -> Result<(), Box<dyn std::error::Error>> {
        let s1: AuthSessionId = "s1".parse()?;
        let s2: AuthSessionId = "s2".parse()?;
        let cid = ConnectionId::generate()?;
        let user = sweep(SweepScope::User, 2)?;
        assert_eq!(fit(&user, &grant(&s1, cid, 1, 1)?), GrantFit::Obsolete);
        assert_eq!(fit(&user, &grant(&s1, cid, 2, 1)?), GrantFit::Current);
        let session = sweep(SweepScope::Session { sid: s1.clone() }, 2)?;
        assert_eq!(fit(&session, &grant(&s1, cid, 1, 1)?), GrantFit::Obsolete);
        assert_eq!(fit(&session, &grant(&s2, cid, 1, 1)?), GrantFit::Outside);
        let refresh = sweep(
            SweepScope::Connection {
                sid: s1.clone(),
                cid,
                below: AuthVersion::new(2),
            },
            1,
        )?;
        assert_eq!(fit(&refresh, &grant(&s1, cid, 1, 1)?), GrantFit::Obsolete);
        assert_eq!(fit(&refresh, &grant(&s1, cid, 1, 2)?), GrantFit::Current);
        assert_eq!(
            fit(&refresh, &grant(&s1, ConnectionId::generate()?, 1, 1)?),
            GrantFit::Outside
        );
        let mut other = grant(&s1, cid, 1, 1)?;
        other.sub = "bob".to_owned().try_into()?;
        assert_eq!(fit(&user, &other), GrantFit::Outside);
        Ok(())
    }

    #[test]
    fn roster_and_bounds_reject_empty_values() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(ServerRoster::new(Vec::new()), Err(ServerRosterError));
        let server: ServerNkey = nkeys::KeyPair::new_server().public_key().parse()?;
        let roster = ServerRoster::new([server.clone(), server])?;
        assert_eq!(roster.servers().len(), 1);
        assert!(ConnzPageLimit::try_from(0).is_err());
        assert!(SweepInterval::try_from(Duration::ZERO).is_err());
        assert_eq!(SweepInterval::default().get(), Duration::from_secs(3));
        Ok(())
    }

    #[test]
    fn replies_from_another_server_do_not_count() -> Result<(), Box<dyn std::error::Error>> {
        let server: ServerNkey = nkeys::KeyPair::new_server().public_key().parse()?;
        let other = nkeys::KeyPair::new_server().public_key();
        let payload = format!(r#"{{"server":{{"id":"{other}"}},"data":{{"total":0,"connections":[]}}}}"#);
        assert_eq!(
            SweepExecutor::answer::<ConnzPage>(&server, payload.as_bytes()).err(),
            Some(ServerGap::WrongServer)
        );
        let payload = format!(r#"{{"server":{{"id":"{server}"}},"error":{{"code":403,"description":"no"}}}}"#);
        assert!(matches!(
            SweepExecutor::answer::<ConnzPage>(&server, payload.as_bytes()),
            Err(ServerGap::Error { code: 403, .. })
        ));
        Ok(())
    }
}
