use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::canonical_json::{canonical_bytes, FingerprintBuilder, OperationFingerprint};
use crate::config::LeaseTtl;
use crate::constants::{CLOCK_SKEW_ALLOWANCE, RETRY_WINDOW_LEASE_DIVISOR, RETRY_WINDOW_MAX};
use crate::holder::HolderId;
use crate::key::PresenceKey;
use crate::meta::Meta;
use crate::position::{decimal_u64, LifetimeId, MutationSequence, OperationId};
use crate::topic::Topic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(#[serde(with = "decimal_u64")] u64);

impl UnixMillis {
    pub fn now() -> Self {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Self(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn saturating_add(self, span: Duration) -> Self {
        Self(
            self.0
                .saturating_add(u64::try_from(span.as_millis()).unwrap_or(u64::MAX)),
        )
    }
}

impl From<u64> for UnixMillis {
    fn from(millis: u64) -> Self {
        Self(millis)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryWindow(Duration);

impl RetryWindow {
    pub fn for_ttl(ttl: LeaseTtl) -> Self {
        Self(RETRY_WINDOW_MAX.min(ttl.get() / RETRY_WINDOW_LEASE_DIVISOR))
    }

    pub fn get(self) -> Duration {
        self.0
    }

    pub fn open(self, issued_at: UnixMillis) -> RequestWindow {
        RequestWindow {
            issued_at,
            deadline: issued_at.saturating_add(self.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestWindow {
    issued_at: UnixMillis,
    deadline: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("the retry window is empty, too wide or issued in the future")]
    InvalidRetryWindow,
    #[error("the retry window has passed")]
    Expired,
    #[error("the request names a different holder")]
    WrongHolder,
}

impl RequestWindow {
    pub fn new(issued_at: UnixMillis, deadline: UnixMillis) -> Self {
        Self { issued_at, deadline }
    }

    pub fn issued_at(self) -> UnixMillis {
        self.issued_at
    }

    pub fn deadline(self) -> UnixMillis {
        self.deadline
    }

    pub(crate) fn admit(self, limit: RetryWindow, now: UnixMillis) -> Result<(), AdmissionError> {
        let span = self.deadline.0.saturating_sub(self.issued_at.0);
        let widest = UnixMillis(0).saturating_add(limit.0).0;
        if span == 0 || span > widest || self.issued_at > now.saturating_add(CLOCK_SKEW_ALLOWANCE) {
            return Err(AdmissionError::InvalidRetryWindow);
        }
        if now > self.deadline.saturating_add(CLOCK_SKEW_ALLOWANCE) {
            return Err(AdmissionError::Expired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Track,
    Update,
    Untrack,
    Release,
}

impl OperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Update => "update",
            Self::Untrack => "untrack",
            Self::Release => "release",
        }
    }

    pub(crate) fn result(self) -> OperationResult {
        match self {
            Self::Track => OperationResult::Tracked,
            Self::Update => OperationResult::Updated,
            Self::Untrack => OperationResult::Untracked,
            Self::Release => OperationResult::Released,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationResult {
    Tracked,
    Updated,
    Untracked,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryTarget {
    lifetime: LifetimeId,
    sequence: MutationSequence,
}

impl EntryTarget {
    pub fn new(lifetime: LifetimeId, sequence: MutationSequence) -> Self {
        Self { lifetime, sequence }
    }

    pub fn lifetime(self) -> LifetimeId {
        self.lifetime
    }

    pub fn sequence(self) -> MutationSequence {
        self.sequence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriteRequest {
    operation_id: OperationId,
    window: RequestWindow,
    holder: HolderId,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IntentBody {
    Track { client_meta: Meta },
    Update { target: EntryTarget, client_meta: Meta },
    Untrack { target: EntryTarget },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteIntent {
    request: WriteRequest,
    topic: Topic,
    key: PresenceKey,
    body: IntentBody,
    fingerprint: OperationFingerprint,
}

impl WriteRequest {
    pub fn new(operation_id: OperationId, window: RequestWindow, holder: HolderId) -> Self {
        Self {
            operation_id,
            window,
            holder,
        }
    }

    pub fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    pub fn window(&self) -> RequestWindow {
        self.window
    }

    pub fn holder(&self) -> HolderId {
        self.holder
    }

    pub fn track(self, topic: Topic, key: PresenceKey, client_meta: Meta) -> WriteIntent {
        WriteIntent::new(self, topic, key, IntentBody::Track { client_meta })
    }

    pub fn update(self, topic: Topic, key: PresenceKey, target: EntryTarget, client_meta: Meta) -> WriteIntent {
        WriteIntent::new(self, topic, key, IntentBody::Update { target, client_meta })
    }

    pub fn untrack(self, topic: Topic, key: PresenceKey, target: EntryTarget) -> WriteIntent {
        WriteIntent::new(self, topic, key, IntentBody::Untrack { target })
    }

    pub fn release(self, key: PresenceKey, targets: Vec<ReleaseTarget>) -> ReleaseIntent {
        ReleaseIntent::new(self, key, targets)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReleaseTarget {
    topic: Topic,
    lifetime: LifetimeId,
}

impl ReleaseTarget {
    pub fn new(topic: Topic, lifetime: LifetimeId) -> Self {
        Self { topic, lifetime }
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseIntent {
    request: WriteRequest,
    key: PresenceKey,
    targets: Vec<ReleaseTarget>,
    fingerprint: OperationFingerprint,
}

impl ReleaseIntent {
    fn new(request: WriteRequest, key: PresenceKey, mut targets: Vec<ReleaseTarget>) -> Self {
        targets.sort_by(|a, b| {
            a.topic
                .cmp(&b.topic)
                .then_with(|| a.lifetime.as_bytes().cmp(b.lifetime.as_bytes()))
        });
        targets.dedup();
        let mut builder = FingerprintBuilder::new()
            .field(request.operation_id.as_bytes())
            .field(OperationKind::Release.as_str().as_bytes())
            .field(request.holder.as_bytes())
            .field(key.as_str().as_bytes())
            .field(&request.window.issued_at.0.to_be_bytes())
            .field(&request.window.deadline.0.to_be_bytes());
        for target in &targets {
            builder = builder
                .field(target.topic.as_str().as_bytes())
                .field(target.lifetime.as_bytes());
        }
        Self {
            request,
            key,
            targets,
            fingerprint: builder.finish(),
        }
    }

    pub fn request(&self) -> &WriteRequest {
        &self.request
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn targets(&self) -> &[ReleaseTarget] {
        &self.targets
    }

    pub fn fingerprint(&self) -> OperationFingerprint {
        self.fingerprint
    }
}

impl IntentBody {
    fn kind(&self) -> OperationKind {
        match self {
            Self::Track { .. } => OperationKind::Track,
            Self::Update { .. } => OperationKind::Update,
            Self::Untrack { .. } => OperationKind::Untrack,
        }
    }

    fn target(&self) -> Option<EntryTarget> {
        match self {
            Self::Track { .. } => None,
            Self::Update { target, .. } | Self::Untrack { target } => Some(*target),
        }
    }

    fn client_meta(&self) -> Option<&Meta> {
        match self {
            Self::Track { client_meta } | Self::Update { client_meta, .. } => Some(client_meta),
            Self::Untrack { .. } => None,
        }
    }
}

fn fingerprint_of(request: &WriteRequest, topic: &Topic, key: &PresenceKey, body: &IntentBody) -> OperationFingerprint {
    let target = body.target().map(|target| {
        let mut bytes = target.lifetime.as_bytes().to_vec();
        bytes.extend_from_slice(&target.sequence.get().to_be_bytes());
        bytes
    });
    let client_meta = body
        .client_meta()
        .map(|meta| canonical_bytes(&serde_json::Value::Object(meta.as_map().clone())));
    FingerprintBuilder::new()
        .field(request.operation_id.as_bytes())
        .field(body.kind().as_str().as_bytes())
        .field(request.holder.as_bytes())
        .field(topic.as_str().as_bytes())
        .field(key.as_str().as_bytes())
        .optional(target.as_deref())
        .field(&request.window.issued_at.0.to_be_bytes())
        .field(&request.window.deadline.0.to_be_bytes())
        .optional(client_meta.as_deref())
        .finish()
}

impl WriteIntent {
    fn new(request: WriteRequest, topic: Topic, key: PresenceKey, body: IntentBody) -> Self {
        let fingerprint = fingerprint_of(&request, &topic, &key, &body);
        Self {
            request,
            topic,
            key,
            body,
            fingerprint,
        }
    }

    pub fn request(&self) -> &WriteRequest {
        &self.request
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn kind(&self) -> OperationKind {
        self.body.kind()
    }

    pub fn target(&self) -> Option<EntryTarget> {
        self.body.target()
    }

    pub fn client_meta(&self) -> Option<&Meta> {
        self.body.client_meta()
    }

    pub fn fingerprint(&self) -> OperationFingerprint {
        self.fingerprint
    }

    pub(crate) fn body(&self) -> &IntentBody {
        &self.body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastOp {
    id: OperationId,
    fingerprint: OperationFingerprint,
    outcome: OperationResult,
}

impl LastOp {
    pub(crate) fn of(intent: &WriteIntent) -> Self {
        Self {
            id: intent.request.operation_id,
            fingerprint: intent.fingerprint,
            outcome: intent.kind().result(),
        }
    }

    pub(crate) fn release(intent: &ReleaseIntent) -> Self {
        Self {
            id: intent.request.operation_id,
            fingerprint: intent.fingerprint,
            outcome: OperationResult::Released,
        }
    }

    pub fn id(&self) -> OperationId {
        self.id
    }

    pub fn fingerprint(&self) -> OperationFingerprint {
        self.fingerprint
    }

    pub fn outcome(&self) -> OperationResult {
        self.outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn request(window: RequestWindow) -> WriteRequest {
        WriteRequest::new(OperationId::from([1; 16]), window, HolderId::from([2; 16]))
    }

    #[test]
    fn retry_window_is_a_third_of_the_lease_capped_at_ten_seconds() -> TestResult {
        let short = LeaseTtl::try_from(Duration::from_secs(3))?;
        assert_eq!(RetryWindow::for_ttl(short).get(), Duration::from_secs(1));
        let long = LeaseTtl::try_from(Duration::from_secs(60))?;
        assert_eq!(RetryWindow::for_ttl(long).get(), RETRY_WINDOW_MAX);
        Ok(())
    }

    #[test]
    fn admission_checks_the_window_before_expiry() {
        let limit = RetryWindow(Duration::from_secs(10));
        let now = UnixMillis(100_000);
        let window = |issued: u64, deadline: u64| RequestWindow::new(UnixMillis(issued), UnixMillis(deadline));
        assert_eq!(window(100_000, 105_000).admit(limit, now), Ok(()));
        assert_eq!(
            window(100_000, 100_000).admit(limit, now),
            Err(AdmissionError::InvalidRetryWindow)
        );
        assert_eq!(
            window(100_000, 110_001).admit(limit, now),
            Err(AdmissionError::InvalidRetryWindow)
        );
        assert_eq!(
            window(101_001, 102_000).admit(limit, now),
            Err(AdmissionError::InvalidRetryWindow)
        );
        assert_eq!(window(101_000, 102_000).admit(limit, now), Ok(()));
        assert_eq!(window(90_000, 98_999).admit(limit, now), Err(AdmissionError::Expired));
        assert_eq!(window(90_000, 99_000).admit(limit, now), Ok(()));
    }

    #[test]
    fn fingerprint_covers_the_complete_intent() -> TestResult {
        let window = RequestWindow::new(UnixMillis(1), UnixMillis(2));
        let meta: Meta = serde_json::from_str(r#"{"a":1,"b":2}"#)?;
        let reordered: Meta = serde_json::from_str(r#"{"b":2,"a":1}"#)?;
        let other: Meta = serde_json::from_str(r#"{"a":2}"#)?;
        let base = request(window).track("room:a".parse()?, "ana".parse()?, meta.clone());
        let same = request(window).track("room:a".parse()?, "ana".parse()?, reordered);
        assert_eq!(base.fingerprint(), same.fingerprint());
        let variants = [
            request(window).track("room:a".parse()?, "ana".parse()?, other),
            request(window).track("room:b".parse()?, "ana".parse()?, meta.clone()),
            request(RequestWindow::new(UnixMillis(1), UnixMillis(3))).track(
                "room:a".parse()?,
                "ana".parse()?,
                meta.clone(),
            ),
            request(window).update(
                "room:a".parse()?,
                "ana".parse()?,
                EntryTarget::new(LifetimeId::from([3; 16]), MutationSequence::FIRST),
                meta,
            ),
        ];
        for variant in variants {
            assert_ne!(variant.fingerprint(), base.fingerprint());
        }
        Ok(())
    }
}
