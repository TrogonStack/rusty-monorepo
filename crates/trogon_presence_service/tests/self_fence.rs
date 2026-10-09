mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;
use trogon_presence::{
    AtomicBatch, BatchSink, ClockControl, HeartbeatInterval, HolderId, KeyCoordinator, LeaseTtl, ManagedError,
    ManagedLimits, MarkerTtl, Meta, NatsBatchSink, OwnerDeadline, OwnerId, Presence, PresenceConfig, PresenceKey,
    ProvisionOptions, SelfFence, SelfFenceBound, ShardCount, SinkFuture, SuspendAwareClock, WriteOutcome, WriterMode,
};
use trogon_presence_service::{NodeId, RequestIdentity, ServiceConfig, ShardLeaseTtl};

use common::{BoxError, NatsServer};

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const OWNER_FENCE: Duration = Duration::from_secs(3);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(Duration::from_secs(3))?,
        HeartbeatInterval::try_from(Duration::from_secs(1))?,
        MarkerTtl::try_from(Duration::from_secs(6))?,
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed))
}

fn meta() -> Result<Meta, BoxError> {
    Ok(serde_json::from_value(json!({ "status": "online" }))?)
}

struct CountingSink {
    inner: NatsBatchSink,
    published: Arc<AtomicUsize>,
}

impl BatchSink for CountingSink {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_> {
        self.published.fetch_add(1, Ordering::SeqCst);
        self.inner.publish(batch)
    }
}

struct Owner {
    coordinator: KeyCoordinator,
    control: ClockControl,
    clock: SuspendAwareClock,
    tenure: watch::Sender<Option<SelfFence>>,
    published: Arc<AtomicUsize>,
    key: PresenceKey,
    holder: HolderId,
}

impl Owner {
    async fn start(server: &NatsServer) -> Result<Self, BoxError> {
        let client = server.client().await;
        let config = ServiceConfig::new(presence_config()?, "edge-a".parse::<NodeId>()?)
            .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?);
        trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
        let presence = Presence::open(client.clone(), presence_config()?).await?;
        let (clock, control) = SuspendAwareClock::controlled();
        let now = clock.now();
        let fence = SelfFence::confirm(now, now, SelfFenceBound::from(OWNER_FENCE))?;
        let (tenure, fence_rx) = watch::channel(Some(fence));
        let published = Arc::new(AtomicUsize::new(0));
        let key: PresenceKey = "ana".parse()?;
        let mut coordinator = presence
            .coordinator(
                key.clone(),
                OwnerId::generate()?,
                OwnerDeadline::watch(fence_rx, clock.clone()),
                ManagedLimits::default(),
            )?
            .with_sink(Arc::new(CountingSink {
                inner: NatsBatchSink::new(client),
                published: published.clone(),
            }));
        coordinator.ensure_ready().await?;
        Ok(Self {
            coordinator,
            control,
            clock,
            tenure,
            published,
            key,
            holder: HolderId::generate()?,
        })
    }

    async fn track(&mut self) -> Result<Result<WriteOutcome, ManagedError>, BoxError> {
        let command = RequestIdentity::mint(self.coordinator.retry_window())?
            .request(self.holder)
            .track(TOPIC.parse()?, self.key.clone(), meta()?);
        Ok(self.coordinator.submit(command, None, None).await)
    }

    fn published(&self) -> usize {
        self.published.load(Ordering::SeqCst)
    }

    async fn refused(&mut self) -> TestResult {
        let before = self.published();
        let err = self.track().await?.err().ok_or("a fenced owner applied a batch")?;
        assert!(matches!(err, ManagedError::Fenced), "unexpected {err:?}");
        assert_eq!(self.published(), before, "a fenced owner reached the stream");
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewal_inside_the_bound_keeps_the_owner_writing() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    owner.control.advance(OWNER_FENCE - Duration::from_millis(500));
    let sent = owner.clock.now();
    owner.control.advance(Duration::from_millis(50));
    let current = (*owner.tenure.borrow()).ok_or("tenure was released")?;
    let renewed = current.renew(sent, owner.clock.now())?;
    owner.tenure.send_replace(Some(renewed));
    owner.control.advance(Duration::from_secs(1));
    let outcome = owner.track().await??;
    assert!(matches!(outcome, WriteOutcome::Applied(_)), "unexpected {outcome:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_is_refused_once_the_owner_fence_expires() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    owner.control.advance(OWNER_FENCE);
    owner.refused().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewal_acknowledged_across_a_suspend_is_rejected() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    let sent = owner.clock.now();
    owner.control.suspend(Duration::from_millis(500));
    let current = (*owner.tenure.borrow()).ok_or("tenure was released")?;
    assert!(
        current.renew(sent, owner.clock.now()).is_err(),
        "a renewal across a suspend was accepted"
    );
    owner.refused().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backward_clock_step_invalidates_ownership() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    owner.control.step_back(Duration::from_secs(1));
    owner.refused().await
}
