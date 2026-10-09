use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_nats::{Message, Subject};
use futures_util::future::join_all;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use trogon_presence::watch::replay::RebuildBudget;
use trogon_presence::{
    BeatEntry, BeatStatus, KeyCoordinator, LifetimeId, ManagedError, ManagedLimits, MutationSequence, OwnerDeadline,
    OwnerId, Presence, PresenceKey, ReleaseOutcome, ReleaseStatus, Revision, SelfFence, ShardCount, StoredRef,
    StreamGeneration, SuspendAwareClock, Topic, WriteError, WriteOutcome, WriteReceipt, WriterShard,
};

use crate::admission::{AdmissionCounters, Counter, KeyQueueDepth};
use crate::command::{Command, Forwarded};
use crate::config::WriterReplyDeadline;
use crate::inbox::ReplyInbox;
use crate::lease::{LeaseKey, LeaseStore, Renewal};
use crate::reply::{ErrorReply, ReplyCode, Response};
use crate::subjects::internal_write_filter;

const WORKER_TICK: Duration = Duration::from_secs(1);

pub type WriterShards = Arc<Mutex<BTreeSet<WriterShard>>>;

pub(crate) struct WriterContext {
    pub client: async_nats::Client,
    pub presence: Presence,
    pub leases: LeaseStore,
    pub owner: OwnerId,
    pub generation: StreamGeneration,
    pub shards: ShardCount,
    pub limits: ManagedLimits,
    pub key_depth: KeyQueueDepth,
    pub permits: Arc<Semaphore>,
    pub counters: Arc<AdmissionCounters>,
    pub budget: RebuildBudget,
    pub reply_deadline: WriterReplyDeadline,
    pub held: WriterShards,
    pub clock: SuspendAwareClock,
}

struct Job {
    command: Command,
    reply: oneshot::Sender<Response>,
    _permit: OwnedSemaphorePermit,
}

struct WorkerSlot {
    id: u64,
    acquired: Revision,
    sender: mpsc::Sender<Job>,
}

type Workers = Arc<Mutex<HashMap<PresenceKey, WorkerSlot>>>;

struct TenureView {
    shard: WriterShard,
    acquired: Revision,
    deadline: watch::Receiver<Option<SelfFence>>,
}

struct Tenure {
    stop: watch::Sender<bool>,
    keeper: JoinHandle<()>,
    intake: JoinHandle<()>,
}

pub(crate) struct WriterHost {
    context: Arc<WriterContext>,
    tenures: HashMap<WriterShard, Tenure>,
    workers: Workers,
    next_worker: Arc<AtomicU64>,
}

impl WriterHost {
    pub(crate) fn new(context: WriterContext) -> Self {
        Self {
            context: Arc::new(context),
            tenures: HashMap::new(),
            workers: Workers::default(),
            next_worker: Arc::default(),
        }
    }

    pub(crate) async fn acquire_free(&mut self) {
        let lost: Vec<WriterShard> = self
            .tenures
            .iter()
            .filter(|(_, tenure)| tenure.keeper.is_finished())
            .map(|(shard, _)| *shard)
            .collect();
        for shard in lost {
            if let Some(tenure) = self.tenures.remove(&shard) {
                tenure.intake.abort();
            }
            self.context
                .held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&shard);
            tracing::info!(shard = %self.context.shards.token(shard), "writer lease lost");
        }
        let shards = self.context.shards;
        let free: Vec<WriterShard> = shards
            .shards()
            .map(WriterShard::from)
            .filter(|shard| !self.tenures.contains_key(shard))
            .collect();
        let attempts = free.into_iter().map(|shard| {
            let leases = self.context.leases.clone();
            let clock = self.context.clock.clone();
            async move {
                let sent = clock.now();
                let outcome = leases.acquire(LeaseKey::Writer(shard)).await;
                let fence = SelfFence::confirm(sent, clock.now(), leases.ttl().self_fence());
                (shard, fence, outcome)
            }
        });
        for (shard, fence, outcome) in join_all(attempts).await {
            match outcome {
                Ok(Some(revision)) => match fence {
                    Ok(fence) => self.claim(shard, revision, fence).await,
                    Err(breach) => {
                        tracing::warn!(%breach, "writer lease acquired past its self-fence, releasing it");
                        if let Err(err) = self.context.leases.release(LeaseKey::Writer(shard), revision).await {
                            tracing::warn!(%err, "writer lease release failed");
                        }
                    }
                },
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "writer lease acquire failed"),
            }
        }
    }

    async fn claim(&mut self, shard: WriterShard, revision: Revision, fence: SelfFence) {
        match self.start_tenure(shard, revision, fence).await {
            Ok(tenure) => {
                self.tenures.insert(shard, tenure);
                self.context
                    .held
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(shard);
            }
            Err(err) => {
                tracing::warn!(%err, "could not subscribe a writer shard, releasing its lease");
                if let Err(err) = self.context.leases.release(LeaseKey::Writer(shard), revision).await {
                    tracing::warn!(%err, "writer lease release failed");
                }
            }
        }
    }

    async fn start_tenure(
        &self,
        shard: WriterShard,
        acquired: Revision,
        fence: SelfFence,
    ) -> Result<Tenure, async_nats::SubscribeError> {
        let subscriber = self
            .context
            .client
            .subscribe(internal_write_filter(self.context.shards, shard))
            .await?;
        if let Err(err) = self.context.client.flush().await {
            tracing::warn!(%err, "could not flush a writer subscription");
        }
        let (deadline_tx, deadline_rx) = watch::channel(Some(fence));
        let (stop, stop_rx) = watch::channel(false);
        let keeper = tokio::spawn(keep_writer_lease(
            WriterKeeper {
                leases: self.context.leases.clone(),
                clock: self.context.clock.clone(),
                held: self.context.held.clone(),
                shard,
            },
            acquired,
            deadline_tx,
            stop_rx,
        ));
        let view = Arc::new(TenureView {
            shard,
            acquired,
            deadline: deadline_rx,
        });
        let context = self.context.clone();
        let workers = self.workers.clone();
        let next_worker = self.next_worker.clone();
        let intake = tokio::spawn(async move {
            let mut subscriber = subscriber;
            while let Some(message) = subscriber.next().await {
                dispatch(&context, &view, &workers, &next_worker, message).await;
            }
        });
        tracing::info!(shard = %self.context.shards.token(shard), owner_rev = %acquired, "owning writer shard");
        Ok(Tenure { stop, keeper, intake })
    }

    pub(crate) async fn shutdown(self) {
        for (shard, tenure) in self.tenures {
            let _ = tenure.stop.send(true);
            if let Err(err) = tenure.keeper.await {
                tracing::warn!(%err, "writer lease keeper failed");
            }
            tenure.intake.abort();
            self.context
                .held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&shard);
        }
    }
}

struct WriterKeeper {
    leases: LeaseStore,
    clock: SuspendAwareClock,
    held: WriterShards,
    shard: WriterShard,
}

impl WriterKeeper {
    fn drop_tenure(&self, deadline: &watch::Sender<Option<SelfFence>>) {
        let _ = deadline.send(None);
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.shard);
    }

    async fn release(&self, revision: Revision) {
        if let Err(err) = self.leases.release(LeaseKey::Writer(self.shard), revision).await {
            tracing::warn!(%err, "writer lease release failed");
        }
    }
}

async fn keep_writer_lease(
    keeper: WriterKeeper,
    mut revision: Revision,
    deadline: watch::Sender<Option<SelfFence>>,
    mut stop: watch::Receiver<bool>,
) {
    let every = keeper.leases.ttl().renew_every();
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => {
                let _ = deadline.send(None);
                keeper.release(revision).await;
                return;
            }
            _ = tick.tick() => {
                let Some(current) = *deadline.borrow() else {
                    return;
                };
                let remaining = match current.remaining(keeper.clock.now()) {
                    Ok(remaining) => remaining,
                    Err(breach) => {
                        tracing::warn!(%breach, "writer tenure self-fenced");
                        keeper.drop_tenure(&deadline);
                        keeper.release(revision).await;
                        return;
                    }
                };
                let sent = keeper.clock.now();
                let outcome = tokio::select! {
                    outcome = keeper.leases.renew(LeaseKey::Writer(keeper.shard), revision) => outcome,
                    () = tokio::time::sleep(remaining) => {
                        let breach = current.lapse(keeper.clock.now());
                        tracing::warn!(%breach, "writer lease renewal outlived the self-fence");
                        keeper.drop_tenure(&deadline);
                        keeper.release(revision).await;
                        return;
                    }
                };
                match outcome {
                    Ok(Renewal::Renewed(next)) => {
                        revision = next;
                        match current.renew(sent, keeper.clock.now()) {
                            Ok(renewed) => {
                                let _ = deadline.send(Some(renewed));
                            }
                            Err(breach) => {
                                tracing::warn!(%breach, "refusing a writer lease renewal, ownership must be reacquired");
                                keeper.drop_tenure(&deadline);
                                keeper.release(revision).await;
                                return;
                            }
                        }
                    }
                    Ok(Renewal::Lost) => {
                        keeper.drop_tenure(&deadline);
                        return;
                    }
                    Ok(Renewal::Unknown) => tracing::warn!("writer lease renewal outcome unknown"),
                    Err(err) => tracing::warn!(%err, "writer lease renewal failed"),
                }
            }
        }
    }
}

async fn dispatch(
    context: &Arc<WriterContext>,
    tenure: &TenureView,
    workers: &Workers,
    next_worker: &AtomicU64,
    message: Message,
) {
    let envelope: Forwarded = match serde_json::from_slice(&message.payload) {
        Ok(envelope) => envelope,
        Err(err) => {
            if let Some(reply) = message.reply {
                Response::from(ErrorReply::new(ReplyCode::InvalidRequest).detail(err))
                    .send_to(&context.client, reply)
                    .await;
            }
            return;
        }
    };
    let handoff = message
        .reply
        .clone()
        .filter(|handoff| envelope.reply().is_some_and(|caller| &caller != handoff));
    if let Some(handoff) = handoff {
        if let Err(err) = context.client.publish(handoff, Vec::new().into()).await {
            tracing::debug!(%err, "could not confirm a forwarded command hand-off");
        }
    }
    let reply = match envelope.reply() {
        Some(subject) => match ReplyInbox::scoped(envelope.key(), subject) {
            Ok(inbox) => inbox.into_subject(),
            Err(err) => {
                context.counters.bump(Counter::RejectedInbox);
                tracing::debug!(%err, "dropping a forwarded command with an unscoped reply inbox");
                return;
            }
        },
        None => match message.reply {
            Some(reply) => reply,
            None => return,
        },
    };
    if let Err(response) = admit(context, tenure, &envelope) {
        response.send_to(&context.client, reply).await;
        return;
    }
    let Ok(permit) = context.permits.clone().try_acquire_owned() else {
        context.counters.bump(Counter::Overloaded);
        overloaded("writer queue is full").send_to(&context.client, reply).await;
        return;
    };
    let key = envelope.key().clone();
    let (answer, answered) = oneshot::channel();
    let job = Job {
        command: envelope.into_command(),
        reply: answer,
        _permit: permit,
    };
    let sent = {
        let mut map = workers.lock().unwrap_or_else(PoisonError::into_inner);
        let stale = map
            .get(&key)
            .is_none_or(|slot| slot.acquired != tenure.acquired || slot.sender.is_closed());
        let spawned = if stale {
            spawn_worker(context, tenure, workers, next_worker, &key).map(|slot| {
                map.insert(key.clone(), slot);
            })
        } else {
            Ok(())
        };
        match spawned {
            Ok(()) => Ok(match map.get(&key) {
                Some(slot) => slot.sender.try_send(job),
                None => Err(TrySendError::Closed(job)),
            }),
            Err(err) => Err((err, job)),
        }
    };
    let sent = match sent {
        Ok(sent) => sent,
        Err((err, _)) => {
            Response::from(ErrorReply::new(ManagedError::from(err).code().into()))
                .send_to(&context.client, reply)
                .await;
            return;
        }
    };
    match sent {
        Ok(()) => {
            context.counters.bump(Counter::Admitted);
            tokio::spawn(answer_within(
                context.client.clone(),
                reply,
                answered,
                context.reply_deadline,
            ));
        }
        Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
            context.counters.bump(Counter::Overloaded);
            overloaded("key queue is full").send_to(&context.client, reply).await;
        }
    }
}

async fn answer_within(
    client: async_nats::Client,
    reply: Subject,
    answered: oneshot::Receiver<Response>,
    deadline: WriterReplyDeadline,
) {
    awaited(answered, deadline).await.send_to(&client, reply).await;
}

async fn awaited(answered: oneshot::Receiver<Response>, deadline: WriterReplyDeadline) -> Response {
    match tokio::time::timeout(deadline.get(), answered).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => not_ready("writer worker stopped before answering"),
        Err(_) => ErrorReply::new(ReplyCode::Unavailable)
            .outcome_unknown()
            .detail("writer did not answer within the reply deadline")
            .into(),
    }
}

fn admit(context: &WriterContext, tenure: &TenureView, envelope: &Forwarded) -> Result<(), Response> {
    if WriterShard::of(envelope.key(), context.shards) != tenure.shard {
        return Err(not_ready("command was routed to the wrong writer shard"));
    }
    let epoch = envelope.epoch();
    let live = tenure
        .deadline
        .borrow()
        .is_some_and(|fence| fence.remaining(context.clock.now()).is_ok());
    if epoch.owner() != context.owner || epoch.acquired() < tenure.acquired || !live {
        return Err(not_ready("forwarded owner epoch is not the live writer tenure"));
    }
    if envelope.generation() != context.generation {
        return Err(ErrorReply::new(ReplyCode::GenerationChanged).into());
    }
    Ok(())
}

fn spawn_worker(
    context: &Arc<WriterContext>,
    tenure: &TenureView,
    workers: &Workers,
    next_worker: &AtomicU64,
    key: &PresenceKey,
) -> Result<WorkerSlot, trogon_presence::KvKeyError> {
    let deadline = OwnerDeadline::watch(tenure.deadline.clone(), context.clock.clone());
    let coordinator = context
        .presence
        .coordinator(key.clone(), context.owner, deadline.clone(), context.limits)?
        .with_rebuild_budget(context.budget);
    let (sender, receiver) = mpsc::channel(context.key_depth.get());
    let id = next_worker.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(work(
        context.clone(),
        workers.clone(),
        key.clone(),
        id,
        coordinator,
        receiver,
        deadline,
    ));
    Ok(WorkerSlot {
        id,
        acquired: tenure.acquired,
        sender,
    })
}

struct RefreshTask(JoinHandle<()>);

impl Drop for RefreshTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn_refresh(
    coordinator: &KeyCoordinator,
    deadline: OwnerDeadline,
    reply_deadline: WriterReplyDeadline,
) -> RefreshTask {
    let mut refresher = coordinator.refresher();
    RefreshTask(tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + WORKER_TICK, WORKER_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !deadline.is_live() {
                return;
            }
            if refresher.is_idle() {
                continue;
            }
            let key = refresher.key().clone();
            let started = tokio::time::Instant::now();
            if let Err(err) = refresher.refresh().await {
                tracing::warn!(%key, %err, elapsed = ?started.elapsed(), "writer guard refresh failed");
            } else if started.elapsed() > reply_deadline.get() {
                tracing::warn!(%key, elapsed = ?started.elapsed(), "writer guard refresh outlived the reply deadline");
            }
        }
    }))
}

async fn work(
    context: Arc<WriterContext>,
    workers: Workers,
    key: PresenceKey,
    id: u64,
    mut coordinator: KeyCoordinator,
    mut jobs: mpsc::Receiver<Job>,
    deadline: OwnerDeadline,
) {
    let refreshing = spawn_refresh(&coordinator, deadline.clone(), context.reply_deadline);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + WORKER_TICK, WORKER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => {
                    let started = tokio::time::Instant::now();
                    let response = execute(&context, &mut coordinator, job.command).await;
                    if started.elapsed() > context.reply_deadline.get() {
                        tracing::warn!(%key, elapsed = ?started.elapsed(), "writer job outlived the reply deadline");
                    }
                    let _ = job.reply.send(response);
                }
                None => break,
            },
            _ = tick.tick() => {
                if !deadline.is_live() {
                    break;
                }
                if coordinator.is_idle() {
                    let mut map = workers.lock().unwrap_or_else(PoisonError::into_inner);
                    if jobs.is_empty() {
                        if map.get(&key).is_some_and(|slot| slot.id == id) {
                            map.remove(&key);
                        }
                        return;
                    }
                }
            }
        }
    }
    drop(refreshing);
    {
        let mut map = workers.lock().unwrap_or_else(PoisonError::into_inner);
        if map.get(&key).is_some_and(|slot| slot.id == id) {
            map.remove(&key);
        }
    }
    jobs.close();
    while let Some(job) = jobs.recv().await {
        let _ = job.reply.send(not_ready("writer tenure ended"));
    }
}

#[derive(Serialize)]
struct WrittenReply {
    phx_ref: StoredRef,
    rev: Revision,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
    adopted: bool,
}

#[derive(Serialize)]
struct UntrackedReply {
    untracked: bool,
}

#[derive(Serialize)]
pub(crate) struct HeartbeatReply {
    pub interval: u64,
    pub entries: Vec<&'static str>,
}

#[derive(Serialize)]
struct ReleasedReply<'a> {
    topic: &'a Topic,
    lifetime: LifetimeId,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_seq: Option<MutationSequence>,
}

#[derive(Serialize)]
struct ReleaseReply<'a> {
    released: Vec<ReleasedReply<'a>>,
    holder_freed: bool,
    replayed: bool,
}

async fn execute(context: &WriterContext, coordinator: &mut KeyCoordinator, command: Command) -> Response {
    let key = coordinator.key().clone();
    match command {
        Command::Track {
            topic,
            request,
            holder,
            meta,
            enriched,
        } => {
            let intent = request.request(holder).track(topic, key, meta);
            written(coordinator.submit(intent, enriched, None).await)
        }
        Command::Update {
            topic,
            request,
            holder,
            meta,
            enriched,
            target,
            expected_ref,
        } => {
            let intent = request.request(holder).update(topic, key, target.target(), meta);
            written(coordinator.submit(intent, enriched, expected_ref.as_ref()).await)
        }
        Command::Untrack {
            topic,
            request,
            holder,
            target,
        } => {
            let intent = request.request(holder).untrack(topic, key, target.target());
            match coordinator.submit(intent, None, None).await {
                Ok(WriteOutcome::Applied(_) | WriteOutcome::Replayed { .. }) => {
                    Response::ok_json(&UntrackedReply { untracked: true })
                }
                Ok(WriteOutcome::AlreadyLive(_) | WriteOutcome::Gone) => ErrorReply::new(ReplyCode::Gone).into(),
                Err(err) => managed_error(err),
            }
        }
        Command::Heartbeat { entries } => {
            let entries: Vec<BeatEntry> = entries.iter().map(|entry| entry.entry()).collect();
            let statuses = coordinator.heartbeat(&entries).await;
            Response::ok_json(&HeartbeatReply {
                interval: context.presence.config().heartbeat().get().as_secs(),
                entries: statuses.into_iter().map(BeatStatus::as_str).collect(),
            })
        }
        Command::Release {
            request,
            holder,
            targets,
        } => {
            let intent = request
                .request(holder)
                .release(key, targets.iter().map(|target| target.target()).collect());
            match coordinator.release(intent).await {
                Ok(outcome) => released(&outcome),
                Err(err) => managed_error(err),
            }
        }
    }
}

fn written(outcome: Result<WriteOutcome, ManagedError>) -> Response {
    match outcome {
        Ok(WriteOutcome::Applied(receipt) | WriteOutcome::Replayed { receipt, .. }) => written_reply(&receipt),
        Ok(WriteOutcome::AlreadyLive(receipt)) => ErrorReply::new(ReplyCode::AlreadyTracked)
            .current(receipt.stored_ref().clone(), None)
            .entry(receipt.lifetime(), receipt.sequence())
            .into(),
        Ok(WriteOutcome::Gone) => ErrorReply::new(ReplyCode::Gone).into(),
        Err(err) => managed_error(err),
    }
}

fn written_reply(receipt: &WriteReceipt) -> Response {
    Response::ok_json(&WrittenReply {
        phx_ref: receipt.stored_ref().clone(),
        rev: receipt.revision(),
        lifetime: receipt.lifetime(),
        mutation_seq: receipt.sequence(),
        adopted: false,
    })
}

fn released(outcome: &ReleaseOutcome) -> Response {
    Response::ok_json(&ReleaseReply {
        released: outcome
            .entries()
            .iter()
            .map(|entry| {
                let (status, mutation_seq) = match entry.status() {
                    ReleaseStatus::Released(sequence) => ("released", Some(sequence)),
                    ReleaseStatus::Gone => ("gone", None),
                };
                ReleasedReply {
                    topic: entry.topic(),
                    lifetime: entry.lifetime(),
                    status,
                    mutation_seq,
                }
            })
            .collect(),
        holder_freed: outcome.holder_freed(),
        replayed: outcome.replayed(),
    })
}

pub(crate) fn managed_error(err: ManagedError) -> Response {
    let mut reply = ErrorReply::new(err.code().into());
    if let ManagedError::Write(WriteError::Conflict {
        current_ref,
        current_meta,
    }) = &err
    {
        reply = reply.current(current_ref.clone(), Some(current_meta.clone()));
    }
    if err.outcome_unknown() {
        reply = reply.outcome_unknown();
    }
    reply.detail(err).into()
}

fn not_ready(detail: &'static str) -> Response {
    ErrorReply::new(ReplyCode::NotReady).detail(detail).into()
}

fn overloaded(detail: &'static str) -> Response {
    ErrorReply::new(ReplyCode::Overloaded).detail(detail).into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{json, Value};
    use tokio::sync::oneshot;

    use super::{awaited, Response};
    use crate::config::WriterReplyDeadline;

    const SHORT: Duration = Duration::from_millis(20);

    fn deadline() -> WriterReplyDeadline {
        WriterReplyDeadline::try_from(SHORT).expect("a positive deadline")
    }

    fn body(response: &Response) -> Value {
        serde_json::from_slice(response.body()).expect("a json reply")
    }

    #[tokio::test]
    async fn a_job_answering_in_time_passes_its_reply_through() {
        let (answer, answered) = oneshot::channel();
        answer
            .send(Response::ok_json(&json!({ "ok": true })))
            .expect("the receiver is waiting");
        assert_eq!(body(&awaited(answered, deadline()).await), json!({ "ok": true }));
    }

    #[tokio::test]
    async fn a_slow_job_is_answered_unavailable_with_an_unknown_outcome_and_keeps_running() {
        let (answer, answered) = oneshot::channel::<Response>();
        let job = tokio::spawn(async move {
            tokio::time::sleep(SHORT * 5).await;
            answer.is_closed()
        });
        let started = std::time::Instant::now();
        let reply = body(&awaited(answered, deadline()).await);
        assert!(started.elapsed() < SHORT * 5, "the caller waited for the job");
        assert_eq!(reply["error"], "unavailable");
        assert_eq!(reply["outcome_unknown"], true);
        assert_eq!(reply["retryable"], true);
        assert!(
            job.await.expect("the job finished"),
            "the job ran to completion after the deadline"
        );
    }

    #[tokio::test]
    async fn a_worker_that_stops_without_answering_is_not_ready() {
        let (answer, answered) = oneshot::channel::<Response>();
        drop(answer);
        let reply = body(&awaited(answered, deadline()).await);
        assert_eq!(reply["error"], "not_ready");
        assert_eq!(reply["outcome_unknown"], false);
    }

    #[test]
    fn the_reply_deadline_defaults_to_three_seconds_and_rejects_zero() {
        assert_eq!(WriterReplyDeadline::default().get(), Duration::from_secs(3));
        assert!(WriterReplyDeadline::try_from(Duration::ZERO).is_err());
    }
}
