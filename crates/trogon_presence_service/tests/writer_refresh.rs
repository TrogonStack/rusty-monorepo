mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{watch, Notify};
use tokio::time::{timeout, Instant};
use trogon_presence::{
    AtomicBatch, BatchSink, BeatEntry, BeatStatus, ClockControl, HeartbeatInterval, HolderId, KeyCoordinator, LeaseTtl,
    ManagedError, ManagedLimits, MarkerTtl, Meta, NatsBatchSink, OwnerDeadline, OwnerId, Presence, PresenceConfig,
    PresenceKey, ProvisionOptions, RefreshPublishBound, SelfFence, SelfFenceBound, ShardCount, SinkFuture,
    SuspendAwareClock, Topic, WriteOutcome, WriterMode,
};
use trogon_presence_service::{NodeId, ServiceConfig, ShardLeaseTtl, WriterReplyDeadline};

use common::{BoxError, NatsServer};

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const OWNER_FENCE: Duration = Duration::from_secs(60);
const PAST_REFRESH_INTERVAL: Duration = Duration::from_secs(6);
const SIGNAL: Duration = Duration::from_secs(10);

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

#[derive(Clone, Default)]
struct Gate {
    stall: Arc<AtomicBool>,
    stalled: Arc<Notify>,
    release: Arc<Notify>,
    landed: Arc<Notify>,
    guard_only: Arc<AtomicUsize>,
    guarded_writes: Arc<AtomicUsize>,
}

impl Gate {
    fn stall_guard_only_publishes(&self) {
        self.stall.store(true, Ordering::SeqCst);
    }

    fn release(&self) {
        self.stall.store(false, Ordering::SeqCst);
        self.release.notify_one();
    }

    fn guard_only(&self) -> usize {
        self.guard_only.load(Ordering::SeqCst)
    }

    fn guarded_writes(&self) -> usize {
        self.guarded_writes.load(Ordering::SeqCst)
    }
}

struct StallingSink {
    inner: NatsBatchSink,
    gate: Gate,
}

impl BatchSink for StallingSink {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_> {
        if batch.len() != 1 {
            self.gate.guarded_writes.fetch_add(1, Ordering::SeqCst);
            return self.inner.publish(batch);
        }
        self.gate.guard_only.fetch_add(1, Ordering::SeqCst);
        if !self.gate.stall.load(Ordering::SeqCst) {
            return self.inner.publish(batch);
        }
        let inner = self.inner.clone();
        let gate = self.gate.clone();
        let landing = tokio::spawn(async move {
            gate.stalled.notify_one();
            gate.release.notified().await;
            let outcome = inner.publish(batch).await;
            gate.landed.notify_one();
            outcome
        });
        Box::pin(async move { landing.await.expect("the stalled publish task ran") })
    }
}

struct Owner {
    coordinator: KeyCoordinator,
    presence: Presence,
    control: ClockControl,
    clock: SuspendAwareClock,
    _tenure: watch::Sender<Option<SelfFence>>,
    gate: Gate,
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
        let gate = Gate::default();
        let key: PresenceKey = "ana".parse()?;
        let mut coordinator = presence
            .coordinator(
                key.clone(),
                OwnerId::generate()?,
                OwnerDeadline::watch(fence_rx, clock.clone()),
                ManagedLimits::default(),
            )?
            .with_sink(Arc::new(StallingSink {
                inner: NatsBatchSink::new(client),
                gate: gate.clone(),
            }));
        coordinator.ensure_ready().await?;
        Ok(Self {
            coordinator,
            presence,
            control,
            clock,
            _tenure: tenure,
            gate,
            key,
            holder: HolderId::generate()?,
        })
    }

    async fn track(&mut self) -> Result<BeatEntry, BoxError> {
        let topic: Topic = TOPIC.parse()?;
        let command = trogon_presence_service::RequestIdentity::mint(self.coordinator.retry_window())?
            .request(self.holder)
            .track(topic.clone(), self.key.clone(), meta()?);
        match self.coordinator.submit(command, None, None).await? {
            WriteOutcome::Applied(receipt) => Ok(BeatEntry::new(
                self.holder,
                topic,
                receipt.lifetime(),
                receipt.sequence(),
            )),
            other => Err(format!("track did not apply: {other:?}").into()),
        }
    }

    async fn heartbeat(&mut self, entry: &BeatEntry) -> Result<Vec<BeatStatus>, BoxError> {
        let bound = WriterReplyDeadline::default().get();
        let load = load_average();
        let started = Instant::now();
        let statuses = timeout(bound, self.coordinator.heartbeat(std::slice::from_ref(entry)))
            .await
            .map_err(|_| format!("heartbeat outlived the {bound:?} reply bound at load {load}"))?;
        eprintln!("heartbeat answered in {:?} at 1m load {load}", started.elapsed());
        Ok(statuses)
    }
}

fn load_average() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|loads| loads.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_refresh_never_delays_a_heartbeat_on_its_key() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    let entry = owner.track().await?;
    owner.gate.stall_guard_only_publishes();
    owner.control.advance(PAST_REFRESH_INTERVAL);
    let mut refresher = owner.coordinator.refresher();
    let refreshing = tokio::spawn(async move { refresher.refresh().await });
    timeout(SIGNAL, owner.gate.stalled.notified()).await?;

    assert_eq!(owner.heartbeat(&entry).await?, vec![BeatStatus::Ok]);
    let preempted = timeout(SIGNAL, refreshing).await??;
    assert!(preempted.is_ok(), "a preempted refresh reported {preempted:?}");

    owner.gate.release();
    timeout(SIGNAL, owner.gate.landed.notified()).await?;
    assert_eq!(owner.heartbeat(&entry).await?, vec![BeatStatus::Ok]);
    assert!(owner.coordinator.is_ready());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_that_outlives_its_bound_makes_the_next_write_confirm_first() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    let entry = owner.track().await?;
    owner.gate.stall_guard_only_publishes();
    owner.control.advance(PAST_REFRESH_INTERVAL);
    let mut refresher = owner
        .coordinator
        .refresher()
        .with_publish_bound(RefreshPublishBound::try_from(Duration::from_millis(200))?);
    let stalled = refresher.refresh().await;
    assert!(
        matches!(stalled, Err(ManagedError::OutcomeUnknown)),
        "unexpected {stalled:?}"
    );

    owner.gate.release();
    timeout(SIGNAL, owner.gate.landed.notified()).await?;
    let writes = owner.gate.guarded_writes();
    assert_eq!(owner.heartbeat(&entry).await?, vec![BeatStatus::Ok]);
    assert_eq!(
        owner.gate.guarded_writes(),
        writes + 1,
        "the heartbeat raced the landed refresh instead of confirming the guard first"
    );
    assert!(owner.coordinator.is_ready());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_observes_a_guard_the_refresh_task_found_invalid() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    let entry = owner.track().await?;
    let mut foreign = owner.presence.coordinator(
        owner.key.clone(),
        OwnerId::generate()?,
        OwnerDeadline::unbounded(owner.clock.clone()),
        ManagedLimits::default(),
    )?;
    foreign.ensure_ready().await?;
    owner.control.advance(PAST_REFRESH_INTERVAL);

    let mut refresher = owner.coordinator.refresher();
    let invalid = refresher.refresh().await;
    assert!(
        matches!(invalid, Err(ManagedError::Superseded)),
        "unexpected {invalid:?}"
    );
    assert!(!refresher.is_ready());
    assert!(
        !owner.coordinator.is_ready(),
        "the job side still saw a guard the refresh task invalidated"
    );

    let rebuilds = owner.gate.guard_only();
    assert_eq!(owner.heartbeat(&entry).await?, vec![BeatStatus::Ok]);
    assert!(
        owner.gate.guard_only() >= rebuilds + 2,
        "the next job wrote on the invalidated guard instead of rebuilding it"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_skips_a_guard_a_job_adopted_inside_the_refresh_interval() -> TestResult {
    let server = server_or_skip!();
    let mut owner = Owner::start(&server).await?;
    owner.track().await?;
    let mut refresher = owner.coordinator.refresher();
    let before = owner.gate.guard_only();

    owner.control.advance(Duration::from_secs(1));
    refresher.refresh().await?;
    assert_eq!(owner.gate.guard_only(), before, "a fresh guard was refreshed");

    owner.control.advance(PAST_REFRESH_INTERVAL - Duration::from_secs(1));
    refresher.refresh().await?;
    assert_eq!(owner.gate.guard_only(), before + 1, "a stale guard was not refreshed");
    Ok(())
}
