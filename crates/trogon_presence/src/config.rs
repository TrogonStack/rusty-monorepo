use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::stream::StorageType;

use crate::batch::InflightBatchLimit;
use crate::bucket::WriterMode;
use crate::constants::{
    BATCH_PROBE_STREAM_PREFIX, DEFAULT_BUCKET, DEFAULT_GUARD_TTL_SECS, DEFAULT_HEARTBEAT_INTERVAL_SECS,
    DEFAULT_LEASE_TTL_SECS, DEFAULT_MARKER_TTL_SECS, KV_STREAM_PREFIX, KV_SUBJECT_PREFIX,
    LEASE_TTL_TO_HEARTBEAT_DENOMINATOR, LEASE_TTL_TO_HEARTBEAT_NUMERATOR, MARKER_TTL_TO_LEASE_TTL_FACTOR, RECEIPT_TTL,
    REPLICAS_MAX, TOKEN_SEPARATOR,
};
use crate::domain::{JetStreamDomain, JetStreamRoute};
use crate::kv_key::KvKey;
use crate::read::ReadRequestTimeout;
use crate::shard::ShardCount;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BucketName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bucket name {0:?} must be non-empty and contain only A-Z, a-z, 0-9, _ and -")]
pub struct BucketNameError(String);

impl BucketName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn stream_name(&self) -> String {
        format!("{KV_STREAM_PREFIX}{}", self.0)
    }

    pub fn subject_for(&self, key: &KvKey) -> String {
        format!("{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{}{TOKEN_SEPARATOR}{key}", self.0)
    }

    pub fn subjects_filter(&self) -> String {
        format!("{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{}{TOKEN_SEPARATOR}>", self.0)
    }

    pub fn probe_stream_name(&self) -> String {
        format!("{BATCH_PROBE_STREAM_PREFIX}{}", self.0)
    }

    pub fn probe_subject_root(&self) -> String {
        format!("{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{}", self.probe_stream_name())
    }
}

impl Default for BucketName {
    fn default() -> Self {
        Self(DEFAULT_BUCKET.to_owned())
    }
}

impl TryFrom<String> for BucketName {
    type Error = BucketNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(BucketNameError(value))
        }
    }
}

impl FromStr for BucketName {
    type Err = BucketNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl fmt::Display for BucketName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for BucketName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{what} must be a whole number of seconds and at least one second, got {value:?}")]
pub struct WholeSecondsError {
    what: &'static str,
    value: Duration,
}

fn whole_seconds(what: &'static str, value: Duration) -> Result<Duration, WholeSecondsError> {
    if value.subsec_nanos() == 0 && value.as_secs() >= 1 {
        Ok(value)
    } else {
        Err(WholeSecondsError { what, value })
    }
}

fn ttl_header(value: Duration) -> String {
    format!("{}s", value.as_secs())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseTtl(Duration);

impl LeaseTtl {
    pub fn get(self) -> Duration {
        self.0
    }

    pub fn header_value(self) -> String {
        ttl_header(self.0)
    }
}

impl Default for LeaseTtl {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_LEASE_TTL_SECS))
    }
}

impl TryFrom<Duration> for LeaseTtl {
    type Error = WholeSecondsError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        whole_seconds("lease ttl", value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerTtl(Duration);

impl MarkerTtl {
    pub fn get(self) -> Duration {
        self.0
    }

    pub fn header_value(self) -> String {
        ttl_header(self.0)
    }
}

impl Default for MarkerTtl {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_MARKER_TTL_SECS))
    }
}

impl TryFrom<Duration> for MarkerTtl {
    type Error = WholeSecondsError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        whole_seconds("marker ttl", value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuardTtl(Duration);

impl GuardTtl {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for GuardTtl {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_GUARD_TTL_SECS))
    }
}

impl TryFrom<Duration> for GuardTtl {
    type Error = WholeSecondsError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        whole_seconds("guard ttl", value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceiptTtl(Duration);

impl ReceiptTtl {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for ReceiptTtl {
    fn default() -> Self {
        Self(RECEIPT_TTL)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageTtl(Duration);

impl MessageTtl {
    pub fn get(self) -> Duration {
        self.0
    }

    pub fn header_value(self) -> String {
        ttl_header(self.0)
    }
}

impl TryFrom<Duration> for MessageTtl {
    type Error = WholeSecondsError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        whole_seconds("message ttl", value).map(Self)
    }
}

impl From<LeaseTtl> for MessageTtl {
    fn from(ttl: LeaseTtl) -> Self {
        Self(ttl.0)
    }
}

impl From<MarkerTtl> for MessageTtl {
    fn from(ttl: MarkerTtl) -> Self {
        Self(ttl.0)
    }
}

impl From<ReceiptTtl> for MessageTtl {
    fn from(ttl: ReceiptTtl) -> Self {
        Self(ttl.0)
    }
}

impl From<GuardTtl> for MessageTtl {
    fn from(ttl: GuardTtl) -> Self {
        Self(ttl.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeartbeatInterval(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("heartbeat interval must be greater than zero")]
pub struct HeartbeatIntervalError;

impl HeartbeatInterval {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for HeartbeatInterval {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_HEARTBEAT_INTERVAL_SECS))
    }
}

impl TryFrom<Duration> for HeartbeatInterval {
    type Error = HeartbeatIntervalError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(HeartbeatIntervalError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Replicas(u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("replicas must be between 1 and {REPLICAS_MAX}, got {0}")]
pub struct ReplicasError(u8);

impl Replicas {
    pub const SINGLE: Self = Self(1);

    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for Replicas {
    fn default() -> Self {
        Self::SINGLE
    }
}

impl TryFrom<u8> for Replicas {
    type Error = ReplicasError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (1..=REPLICAS_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ReplicasError(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("lease ttl {ttl:?} must be at least 2.5 times the heartbeat interval {interval:?}")]
    LeaseTtlTooShort { ttl: Duration, interval: Duration },
    #[error("marker ttl {marker:?} must be at least twice the lease ttl {ttl:?}")]
    MarkerTtlTooShort { marker: Duration, ttl: Duration },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceConfig {
    bucket: BucketName,
    lease_ttl: LeaseTtl,
    heartbeat: HeartbeatInterval,
    marker_ttl: MarkerTtl,
    shards: ShardCount,
    writer_mode: WriterMode,
    route: JetStreamRoute,
    inflight_batches: InflightBatchLimit,
    read_timeout: ReadRequestTimeout,
}

impl PresenceConfig {
    pub fn new(
        bucket: BucketName,
        lease_ttl: LeaseTtl,
        heartbeat: HeartbeatInterval,
        marker_ttl: MarkerTtl,
        shards: ShardCount,
    ) -> Result<Self, ConfigError> {
        if lease_ttl.get() * LEASE_TTL_TO_HEARTBEAT_DENOMINATOR < heartbeat.get() * LEASE_TTL_TO_HEARTBEAT_NUMERATOR {
            return Err(ConfigError::LeaseTtlTooShort {
                ttl: lease_ttl.get(),
                interval: heartbeat.get(),
            });
        }
        if marker_ttl.get() < lease_ttl.get() * MARKER_TTL_TO_LEASE_TTL_FACTOR {
            return Err(ConfigError::MarkerTtlTooShort {
                marker: marker_ttl.get(),
                ttl: lease_ttl.get(),
            });
        }
        Ok(Self {
            bucket,
            lease_ttl,
            heartbeat,
            marker_ttl,
            shards,
            writer_mode: WriterMode::default(),
            route: JetStreamRoute::local(),
            inflight_batches: InflightBatchLimit::default(),
            read_timeout: ReadRequestTimeout::default(),
        })
    }

    pub fn with_writer_mode(self, writer_mode: WriterMode) -> Self {
        Self { writer_mode, ..self }
    }

    pub fn with_domain(self, domain: JetStreamDomain) -> Self {
        Self {
            route: JetStreamRoute::through(domain),
            ..self
        }
    }

    pub fn with_inflight_batches(self, inflight_batches: InflightBatchLimit) -> Self {
        Self {
            inflight_batches,
            ..self
        }
    }

    pub fn with_read_timeout(self, read_timeout: ReadRequestTimeout) -> Self {
        Self { read_timeout, ..self }
    }

    pub fn read_timeout(&self) -> ReadRequestTimeout {
        self.read_timeout
    }

    pub fn route(&self) -> &JetStreamRoute {
        &self.route
    }

    pub fn inflight_batches(&self) -> InflightBatchLimit {
        self.inflight_batches
    }

    pub fn context(&self, client: async_nats::Client) -> async_nats::jetstream::Context {
        self.route.context(client)
    }

    pub fn writer_mode(&self) -> WriterMode {
        self.writer_mode
    }

    pub fn bucket(&self) -> &BucketName {
        &self.bucket
    }

    pub fn lease_ttl(&self) -> LeaseTtl {
        self.lease_ttl
    }

    pub fn heartbeat(&self) -> HeartbeatInterval {
        self.heartbeat
    }

    pub fn marker_ttl(&self) -> MarkerTtl {
        self.marker_ttl
    }

    pub fn shards(&self) -> ShardCount {
        self.shards
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionOptions {
    pub storage: StorageType,
    pub replicas: Replicas,
}

impl Default for ProvisionOptions {
    fn default() -> Self {
        Self {
            storage: StorageType::File,
            replicas: Replicas::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_names_derive_from_the_bucket() -> Result<(), BucketNameError> {
        let bucket = BucketName::try_from("PRESENCE_V1".to_owned())?;
        assert_eq!(bucket.probe_stream_name(), "PRESENCE_PROBE_PRESENCE_V1");
        assert_eq!(bucket.probe_subject_root(), "$KV.PRESENCE_PROBE_PRESENCE_V1");
        Ok(())
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn default_config_is_valid() -> TestResult {
        let defaults = PresenceConfig::default();
        let rebuilt = PresenceConfig::new(
            defaults.bucket().clone(),
            defaults.lease_ttl(),
            defaults.heartbeat(),
            defaults.marker_ttl(),
            defaults.shards(),
        )?;
        assert_eq!(rebuilt, defaults);
        assert_eq!(defaults.bucket().as_str(), "PRESENCE_V1");
        assert_eq!(defaults.lease_ttl().header_value(), "30s");
        Ok(())
    }

    #[test]
    fn enforces_ttl_ratios() -> TestResult {
        let ttl = LeaseTtl::try_from(Duration::from_secs(25))?;
        let interval = HeartbeatInterval::try_from(Duration::from_secs(10))?;
        assert!(PresenceConfig::new(
            BucketName::default(),
            ttl,
            interval,
            MarkerTtl::default(),
            ShardCount::DEFAULT
        )
        .is_ok());
        let short = LeaseTtl::try_from(Duration::from_secs(24))?;
        assert!(matches!(
            PresenceConfig::new(
                BucketName::default(),
                short,
                interval,
                MarkerTtl::default(),
                ShardCount::DEFAULT
            ),
            Err(ConfigError::LeaseTtlTooShort { .. })
        ));
        let marker = MarkerTtl::try_from(Duration::from_secs(49))?;
        assert!(matches!(
            PresenceConfig::new(BucketName::default(), ttl, interval, marker, ShardCount::DEFAULT),
            Err(ConfigError::MarkerTtlTooShort { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_fractional_and_zero_ttls() {
        assert!(LeaseTtl::try_from(Duration::from_millis(1500)).is_err());
        assert!(LeaseTtl::try_from(Duration::ZERO).is_err());
        assert!(HeartbeatInterval::try_from(Duration::ZERO).is_err());
        assert!(Replicas::try_from(0).is_err());
        assert!(Replicas::try_from(6).is_err());
    }

    #[test]
    fn a_config_routes_locally_until_given_a_domain() -> TestResult {
        let local = PresenceConfig::default();
        assert_eq!(local.route(), &JetStreamRoute::local());
        let domain: JetStreamDomain = "hub".parse()?;
        let routed = local.clone().with_domain(domain.clone());
        assert_eq!(routed.route(), &JetStreamRoute::through(domain));
        assert_eq!(routed.route().api_prefix(), "$JS.hub.API");
        assert_eq!(routed.bucket(), local.bucket());
        Ok(())
    }

    #[test]
    fn a_config_carries_its_inflight_batch_limit() -> TestResult {
        let config = PresenceConfig::default();
        assert_eq!(config.inflight_batches(), InflightBatchLimit::default());
        let limit = InflightBatchLimit::try_from(4)?;
        let limited = config.clone().with_inflight_batches(limit);
        assert_eq!(limited.inflight_batches(), limit);
        assert_eq!(limited.route(), config.route());
        assert!(InflightBatchLimit::try_from(0).is_err());
        assert!("0".parse::<InflightBatchLimit>().is_err());
        Ok(())
    }

    #[test]
    fn a_config_carries_its_read_request_timeout() -> TestResult {
        let config = PresenceConfig::default();
        assert_eq!(config.read_timeout(), ReadRequestTimeout::default());
        let timeout: ReadRequestTimeout = "250ms".parse()?;
        let shortened = config.clone().with_read_timeout(timeout);
        assert_eq!(shortened.read_timeout(), timeout);
        assert_eq!(shortened.inflight_batches(), config.inflight_batches());
        Ok(())
    }

    #[test]
    fn validates_bucket_names() -> TestResult {
        let bucket: BucketName = "PRESENCE_V1".parse()?;
        assert_eq!(bucket.stream_name(), "KV_PRESENCE_V1");
        let key = KvKey::try_from("s56.ana.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby".to_owned())?;
        assert_eq!(
            bucket.subject_for(&key),
            "$KV.PRESENCE_V1.s56.ana.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby"
        );
        for bad in ["", "a.b", "a b", "a*", "a>"] {
            assert!(bad.parse::<BucketName>().is_err(), "{bad}");
        }
        Ok(())
    }
}
