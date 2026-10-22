use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use trogon_presence::watch::replay::{RebuildBudget, ReconcileInterval};
use trogon_presence::{
    BucketName, CoalesceWindow, InflightBatchLimit, ManagedLimits, PresenceConfig, ReadRequestTimeout, SelfFenceBound,
    WriterMode,
};
use trogon_presence_hooks::HookConfig;

use crate::admission::AdmissionLimits;
use crate::snapshot::SnapshotLimits;

pub const DEFAULT_LEASE_BUCKET: &str = "PRESENCE_LEASE_V1";
const DEFAULT_SHARD_LEASE_TTL_SECS: u64 = 15;
const SHARD_LEASE_TTL_MIN_SECS: u64 = 3;
const SHARD_LEASE_RENEW_DIVISOR: u32 = 3;
const SHARD_LEASE_RENEWAL_SLACK_DIVISOR: u32 = 5;
const DEFAULT_KEEPALIVE_SECS: u64 = 30;
const DEFAULT_WRITER_REPLY_DEADLINE_SECS: u64 = 3;
const NODE_ID_MAX_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardLeaseTtl(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shard lease ttl must be a whole number of seconds and at least {SHARD_LEASE_TTL_MIN_SECS}s, got {0:?}")]
pub struct ShardLeaseTtlError(Duration);

impl ShardLeaseTtl {
    pub fn get(self) -> Duration {
        self.0
    }

    pub fn renew_every(self) -> Duration {
        self.0 / SHARD_LEASE_RENEW_DIVISOR
    }

    pub fn fence_after(self) -> Duration {
        self.renew_every() + self.renewal_slack()
    }

    pub fn renewal_slack(self) -> Duration {
        self.0 / SHARD_LEASE_RENEWAL_SLACK_DIVISOR
    }

    pub fn self_fence(self) -> SelfFenceBound {
        SelfFenceBound::from(self.fence_after())
    }

    pub fn header_value(self) -> String {
        format!("{}s", self.0.as_secs())
    }
}

impl Default for ShardLeaseTtl {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_SHARD_LEASE_TTL_SECS))
    }
}

impl TryFrom<Duration> for ShardLeaseTtl {
    type Error = ShardLeaseTtlError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.subsec_nanos() == 0 && value.as_secs() >= SHARD_LEASE_TTL_MIN_SECS {
            Ok(Self(value))
        } else {
            Err(ShardLeaseTtlError(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepaliveInterval(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("keepalive interval must be greater than zero")]
pub struct KeepaliveIntervalError;

impl KeepaliveInterval {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for KeepaliveInterval {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_KEEPALIVE_SECS))
    }
}

impl TryFrom<Duration> for KeepaliveInterval {
    type Error = KeepaliveIntervalError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(KeepaliveIntervalError)
        } else {
            Ok(Self(value))
        }
    }
}

/// How long a writer lets a caller wait for a forwarded command before answering
/// `unavailable` with `outcome_unknown`. The command itself keeps running to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WriterReplyDeadline(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("writer reply deadline must be greater than zero")]
pub struct WriterReplyDeadlineError;

impl WriterReplyDeadline {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for WriterReplyDeadline {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_WRITER_REPLY_DEADLINE_SECS))
    }
}

impl TryFrom<Duration> for WriterReplyDeadline {
    type Error = WriterReplyDeadlineError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(WriterReplyDeadlineError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeIdError {
    #[error("node id {0:?} must be 1 to {NODE_ID_MAX_BYTES} bytes of A-Z, a-z, 0-9, _ and -")]
    Invalid(String),
    #[error("could not draw randomness for a node id: {0}")]
    Entropy(String),
}

impl NodeId {
    pub fn generate() -> Result<Self, NodeIdError> {
        let suffix = getrandom::u32().map_err(|err| NodeIdError::Entropy(err.to_string()))?;
        Ok(Self(format!("node-{suffix:08x}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NodeId {
    type Error = NodeIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value.len() <= NODE_ID_MAX_BYTES
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(NodeIdError::Invalid(value))
        }
    }
}

impl FromStr for NodeId {
    type Err = NodeIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    presence: PresenceConfig,
    lease_bucket: BucketName,
    lease_ttl: ShardLeaseTtl,
    keepalive: KeepaliveInterval,
    snapshot: SnapshotLimits,
    coalesce: CoalesceWindow,
    rebuild_budget: RebuildBudget,
    reconcile: ReconcileInterval,
    reply_deadline: WriterReplyDeadline,
    node: NodeId,
    hook: Option<HookConfig>,
    managed: ManagedLimits,
    admission: AdmissionLimits,
}

impl ServiceConfig {
    pub fn new(presence: PresenceConfig, node: NodeId) -> Self {
        Self {
            presence: presence.with_writer_mode(WriterMode::Managed),
            lease_bucket: default_lease_bucket(),
            lease_ttl: ShardLeaseTtl::default(),
            keepalive: KeepaliveInterval::default(),
            snapshot: SnapshotLimits::default(),
            coalesce: CoalesceWindow::default(),
            rebuild_budget: RebuildBudget::default(),
            reconcile: ReconcileInterval::default(),
            reply_deadline: WriterReplyDeadline::default(),
            node,
            hook: None,
            managed: ManagedLimits::default(),
            admission: AdmissionLimits::default(),
        }
    }

    pub fn with_managed_limits(self, managed: ManagedLimits) -> Self {
        Self { managed, ..self }
    }

    pub fn with_admission(self, admission: AdmissionLimits) -> Self {
        Self { admission, ..self }
    }

    pub fn managed_limits(&self) -> ManagedLimits {
        self.managed
    }

    pub fn admission(&self) -> AdmissionLimits {
        self.admission
    }

    pub fn with_inflight_batches(self, inflight_batches: InflightBatchLimit) -> Self {
        Self {
            presence: self.presence.clone().with_inflight_batches(inflight_batches),
            ..self
        }
    }

    pub fn inflight_batches(&self) -> InflightBatchLimit {
        self.presence.inflight_batches()
    }

    pub fn with_read_timeout(self, read_timeout: ReadRequestTimeout) -> Self {
        Self {
            presence: self.presence.clone().with_read_timeout(read_timeout),
            ..self
        }
    }

    pub fn read_timeout(&self) -> ReadRequestTimeout {
        self.presence.read_timeout()
    }

    pub fn with_lease_bucket(self, lease_bucket: BucketName) -> Self {
        Self { lease_bucket, ..self }
    }

    pub fn with_lease_ttl(self, lease_ttl: ShardLeaseTtl) -> Self {
        Self { lease_ttl, ..self }
    }

    pub fn with_keepalive(self, keepalive: KeepaliveInterval) -> Self {
        Self { keepalive, ..self }
    }

    pub fn with_snapshot_limits(self, snapshot: SnapshotLimits) -> Self {
        Self { snapshot, ..self }
    }

    pub fn with_coalesce(self, coalesce: CoalesceWindow) -> Self {
        Self { coalesce, ..self }
    }

    pub fn with_rebuild_budget(self, rebuild_budget: RebuildBudget) -> Self {
        Self { rebuild_budget, ..self }
    }

    pub fn with_reconcile(self, reconcile: ReconcileInterval) -> Self {
        Self { reconcile, ..self }
    }

    pub fn with_writer_reply_deadline(self, reply_deadline: WriterReplyDeadline) -> Self {
        Self { reply_deadline, ..self }
    }

    pub fn writer_reply_deadline(&self) -> WriterReplyDeadline {
        self.reply_deadline
    }

    pub fn with_hook(self, hook: HookConfig) -> Self {
        Self {
            hook: Some(hook),
            ..self
        }
    }

    pub fn presence(&self) -> &PresenceConfig {
        &self.presence
    }

    pub fn lease_bucket(&self) -> &BucketName {
        &self.lease_bucket
    }

    pub fn lease_ttl(&self) -> ShardLeaseTtl {
        self.lease_ttl
    }

    pub fn keepalive(&self) -> KeepaliveInterval {
        self.keepalive
    }

    pub fn snapshot_limits(&self) -> SnapshotLimits {
        self.snapshot
    }

    pub fn coalesce(&self) -> CoalesceWindow {
        self.coalesce
    }

    pub fn rebuild_budget(&self) -> RebuildBudget {
        self.rebuild_budget
    }

    pub fn reconcile(&self) -> ReconcileInterval {
        self.reconcile
    }

    pub fn node(&self) -> &NodeId {
        &self.node
    }

    pub fn hook(&self) -> Option<&HookConfig> {
        self.hook.as_ref()
    }
}

pub fn default_lease_bucket() -> BucketName {
    BucketName::try_from(DEFAULT_LEASE_BUCKET.to_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_lease_ttl_derives_renew_and_fence() -> Result<(), ShardLeaseTtlError> {
        let ttl = ShardLeaseTtl::default();
        assert_eq!(ttl.renew_every(), Duration::from_secs(5));
        assert_eq!(ttl.fence_after(), Duration::from_secs(8));
        assert_eq!(ttl.renewal_slack(), Duration::from_secs(3));
        assert_eq!(ttl.header_value(), "15s");
        assert!(ShardLeaseTtl::try_from(Duration::from_secs(2)).is_err());
        assert!(ShardLeaseTtl::try_from(Duration::from_millis(3500)).is_err());
        let shortest = ShardLeaseTtl::try_from(Duration::from_secs(3))?;
        assert_eq!(shortest.renew_every(), Duration::from_secs(1));
        assert_eq!(shortest.renewal_slack(), Duration::from_millis(600));
        assert_eq!(shortest.fence_after(), Duration::from_millis(1600));
        Ok(())
    }

    #[test]
    fn validates_node_ids() -> Result<(), NodeIdError> {
        assert_eq!("edge-1".parse::<NodeId>()?.as_str(), "edge-1");
        for bad in ["", "a.b", "a*", "a b", &"x".repeat(65)] {
            assert!(bad.parse::<NodeId>().is_err(), "{bad}");
        }
        assert!(NodeId::generate()?.as_str().starts_with("node-"));
        Ok(())
    }

    #[test]
    fn forces_managed_writer_mode() -> Result<(), NodeIdError> {
        let config = ServiceConfig::new(PresenceConfig::default(), "edge-1".parse()?);
        assert_eq!(config.presence().writer_mode(), WriterMode::Managed);
        Ok(())
    }

    #[test]
    fn default_lease_bucket_is_versioned() {
        assert_eq!(default_lease_bucket().as_str(), DEFAULT_LEASE_BUCKET);
        assert_eq!(default_lease_bucket().stream_name(), "KV_PRESENCE_LEASE_V1");
    }

    #[test]
    fn inflight_batch_limit_defaults_and_overrides() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServiceConfig::new(PresenceConfig::default(), "edge-1".parse()?);
        assert_eq!(config.inflight_batches(), InflightBatchLimit::default());
        let limit = InflightBatchLimit::try_from(4)?;
        let limited = config.with_inflight_batches(limit);
        assert_eq!(limited.inflight_batches(), limit);
        assert_eq!(limited.presence().inflight_batches(), limit);
        assert_eq!(limited.presence().writer_mode(), WriterMode::Managed);
        Ok(())
    }

    #[test]
    fn read_timeout_defaults_and_overrides() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServiceConfig::new(PresenceConfig::default(), "edge-1".parse()?);
        assert_eq!(config.read_timeout(), ReadRequestTimeout::default());
        let timeout: ReadRequestTimeout = "500ms".parse()?;
        let shortened = config.with_read_timeout(timeout);
        assert_eq!(shortened.read_timeout(), timeout);
        assert_eq!(shortened.presence().read_timeout(), timeout);
        assert_eq!(shortened.presence().writer_mode(), WriterMode::Managed);
        Ok(())
    }

    #[test]
    fn rejects_zero_sizes() {
        assert!(KeepaliveInterval::try_from(Duration::ZERO).is_err());
    }
}
