use std::sync::Arc;
use std::time::Instant;

use async_nats::{HeaderMap, Message, RequestError, RequestErrorKind, Subject};
use futures_util::stream::{select_all, StreamExt};
use serde::Deserialize;
use tokio::sync::{watch, Semaphore};
use trogon_presence::{
    Admission, HolderId, LifetimeId, Meta, MutationSequence, OwnerEpoch, Presence, PresenceKey, RateLimiter,
    RetryWindow, ShardCount, StoredRef, StreamGeneration, Topic, ViewShard, WriterShard,
};
use trogon_presence_hooks::{HookOp, HookOutcome, HookRuntime};

use crate::admission::{AdmissionCounters, AdmissionLimits, Counter};
use crate::command::{BeatWire, Command, EntryRef, Forwarded, ReleaseWire, RequestIdentity};
use crate::config::{NodeId, WriterReplyDeadline};
use crate::heartbeat::HeartbeatMany;
use crate::inbox::ReplyInbox;
use crate::lease::{LeaseKey, LeaseStore};
use crate::reply::{ErrorReply, ReplyCode, Response, HEADER_TOPIC};
use crate::shard_owner::parse_required;
use crate::subjects::{
    caller_token, heartbeat_many_filter, internal_write_subject, parse_snapshot, snapshot_any_shard_filter,
    snapshot_scope, HolderOp, ReadOp, WriteOp, WriteTarget, QUEUE_GROUP,
};

pub const HEARTBEAT_MAX_ENTRIES: usize = 64;
pub const RELEASE_MAX_TARGETS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackBody {
    holder: HolderId,
    #[serde(default)]
    meta: Meta,
    #[serde(default)]
    request: Option<RequestIdentity>,
    #[serde(default)]
    topic: Option<Topic>,
    #[serde(default)]
    key: Option<PresenceKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    holder: HolderId,
    #[serde(default)]
    meta: Meta,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
    #[serde(default)]
    expected_ref: Option<StoredRef>,
    #[serde(default)]
    request: Option<RequestIdentity>,
    #[serde(default)]
    topic: Option<Topic>,
    #[serde(default)]
    key: Option<PresenceKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UntrackBody {
    holder: HolderId,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
    #[serde(default)]
    request: Option<RequestIdentity>,
    #[serde(default)]
    topic: Option<Topic>,
    #[serde(default)]
    key: Option<PresenceKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeartbeatBody {
    entries: Vec<BeatWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseBody {
    holder: HolderId,
    targets: Vec<ReleaseWire>,
    #[serde(default)]
    request: Option<RequestIdentity>,
}

#[derive(Debug, Clone, Copy)]
enum Route {
    Write(WriteOp),
    Redirect(ReadOp),
    SnapshotRedirect,
    Holder(HolderOp),
    HeartbeatMany,
}

pub(crate) struct Ingress {
    pub client: async_nats::Client,
    pub presence: Presence,
    pub leases: LeaseStore,
    pub hook: Option<HookRuntime>,
    pub counters: Arc<AdmissionCounters>,
    pub limits: AdmissionLimits,
    pub reply_deadline: WriterReplyDeadline,
}

#[derive(Clone)]
pub struct Writes {
    inner: Arc<WritesInner>,
}

struct WritesInner {
    client: async_nats::Client,
    shards: ShardCount,
    window: RetryWindow,
    generation: StreamGeneration,
    leases: LeaseStore,
    hook: Option<HookRuntime>,
    key_rate: RateLimiter<String>,
    node_rate: RateLimiter<NodeId>,
    permits: Arc<Semaphore>,
    counters: Arc<AdmissionCounters>,
    heartbeat_secs: u64,
    reply_deadline: WriterReplyDeadline,
}

impl Writes {
    pub(crate) fn new(ingress: Ingress) -> Self {
        let config = ingress.presence.config();
        Self {
            inner: Arc::new(WritesInner {
                shards: config.shards(),
                window: RetryWindow::for_ttl(config.lease_ttl()),
                generation: ingress.presence.generation(),
                heartbeat_secs: config.heartbeat().get().as_secs(),
                client: ingress.client,
                leases: ingress.leases,
                hook: ingress.hook,
                key_rate: RateLimiter::new(ingress.limits.key_rate()),
                node_rate: RateLimiter::new(ingress.limits.node_rate()),
                permits: Arc::new(Semaphore::new(ingress.limits.process().get())),
                counters: ingress.counters,
                reply_deadline: ingress.reply_deadline,
            }),
        }
    }

    pub(crate) fn client(&self) -> &async_nats::Client {
        &self.inner.client
    }

    pub(crate) fn heartbeat_secs(&self) -> u64 {
        self.inner.heartbeat_secs
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> Result<(), async_nats::SubscribeError> {
        let client = self.inner.client.clone();
        let mut streams = Vec::new();
        for op in WriteOp::ALL {
            let subscriber = client.queue_subscribe(op.filter(), QUEUE_GROUP.to_owned()).await?;
            streams.push(subscriber.map(move |message| (Route::Write(op), message)).boxed());
        }
        for op in ReadOp::ALL {
            let subscriber = client
                .queue_subscribe(op.any_shard_filter(), QUEUE_GROUP.to_owned())
                .await?;
            streams.push(subscriber.map(move |message| (Route::Redirect(op), message)).boxed());
        }
        let snapshots = client
            .queue_subscribe(snapshot_any_shard_filter(), QUEUE_GROUP.to_owned())
            .await?;
        streams.push(snapshots.map(|message| (Route::SnapshotRedirect, message)).boxed());
        for op in HolderOp::ALL {
            let subscriber = client.queue_subscribe(op.filter(), QUEUE_GROUP.to_owned()).await?;
            streams.push(subscriber.map(move |message| (Route::Holder(op), message)).boxed());
        }
        let many = client
            .queue_subscribe(heartbeat_many_filter(), QUEUE_GROUP.to_owned())
            .await?;
        streams.push(many.map(|message| (Route::HeartbeatMany, message)).boxed());
        if let Err(err) = client.flush().await {
            tracing::warn!(%err, "could not flush front subscriptions");
        }
        let mut requests = select_all(streams);
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                next = requests.next() => match next {
                    Some((route, message)) => self.admit(route, message).await,
                    None => break,
                },
            }
        }
        Ok(())
    }

    async fn admit(&self, route: Route, message: Message) {
        let inner = &self.inner;
        match route {
            Route::Redirect(op) => {
                let response: Response = match op.parse(&message.subject, inner.shards) {
                    Ok(target) => {
                        ErrorReply::not_owner(inner.shards, ViewShard::of(target.topic(), inner.shards)).into()
                    }
                    Err(err) => ErrorReply::new(err.code()).detail(err).into(),
                };
                response.send(&inner.client, &message).await;
            }
            Route::SnapshotRedirect => {
                let inbox = message.reply.clone().and_then(|reply| {
                    let (key, connection) = snapshot_scope(message.subject.as_str())?;
                    ReplyInbox::under_connection_token(key, connection, reply).ok()
                });
                let Some(inbox) = inbox else {
                    inner.counters.bump(Counter::RejectedInbox);
                    return;
                };
                let response: Response = match parse_snapshot(&message.subject, inner.shards) {
                    Ok(target) => {
                        ErrorReply::not_owner(inner.shards, ViewShard::of(target.read().topic(), inner.shards)).into()
                    }
                    Err(err) => ErrorReply::new(err.code()).detail(err).into(),
                };
                response.send_to(&inner.client, inbox.into_subject()).await;
            }
            Route::HeartbeatMany => {
                let Some(reply) = message.reply.clone() else {
                    return;
                };
                let limited = match crate::subjects::heartbeat_many_node(&message.subject) {
                    Ok(node) => inner.node_rate.check(&node, Instant::now()) == Admission::Limited,
                    Err(err) => {
                        Response::from(ErrorReply::new(err.code()).detail(err))
                            .send_to(&inner.client, reply)
                            .await;
                        return;
                    }
                };
                if limited {
                    inner.counters.bump(Counter::RateLimited);
                    overloaded("node heartbeat rate exceeded")
                        .send_to(&inner.client, reply)
                        .await;
                    return;
                }
                let Ok(permit) = inner.permits.clone().try_acquire_owned() else {
                    inner.counters.bump(Counter::Overloaded);
                    overloaded("ingress queue is full").send_to(&inner.client, reply).await;
                    return;
                };
                let writes = self.clone();
                tokio::spawn(async move {
                    let response = HeartbeatMany::new(&writes).handle(&message.payload).await;
                    response.send_to(&writes.inner.client, reply).await;
                    drop(permit);
                });
            }
            Route::Write(_) | Route::Holder(_) => {
                let token = caller_token(message.subject.as_str()).unwrap_or_default().to_owned();
                let limited = inner.key_rate.check(&token, Instant::now()) == Admission::Limited;
                let inbox = match message
                    .reply
                    .clone()
                    .map(|reply| ReplyInbox::under_token(&token, reply))
                {
                    Some(Ok(inbox)) => inbox,
                    Some(Err(err)) => {
                        inner.counters.bump(Counter::RejectedInbox);
                        tracing::debug!(%err, "dropping a command with an unscoped reply inbox");
                        return;
                    }
                    None => {
                        inner.counters.bump(Counter::RejectedInbox);
                        return;
                    }
                };
                if limited {
                    inner.counters.bump(Counter::RateLimited);
                    overloaded("key command rate exceeded")
                        .send_to(&inner.client, inbox.into_subject())
                        .await;
                    return;
                }
                let Ok(permit) = inner.permits.clone().try_acquire_owned() else {
                    inner.counters.bump(Counter::Overloaded);
                    overloaded("ingress queue is full")
                        .send_to(&inner.client, inbox.into_subject())
                        .await;
                    return;
                };
                let writes = self.clone();
                tokio::spawn(async move {
                    if let Err(response) = writes.handle(route, &message, &inbox).await {
                        response.send_to(&writes.inner.client, inbox.into_subject()).await;
                    }
                    drop(permit);
                });
            }
        }
    }

    async fn handle(&self, route: Route, message: &Message, inbox: &ReplyInbox) -> Result<(), Response> {
        let reply = Some(inbox.subject().clone());
        match route {
            Route::Write(op) => {
                let target = op
                    .parse(&message.subject)
                    .map_err(|err| Response::from(ErrorReply::new(err.code()).detail(err)))?;
                check_header(message.headers.as_ref(), target.topic())?;
                let command = match op {
                    WriteOp::Track => self.track(&target, &message.payload).await?,
                    WriteOp::Update => self.update(&target, &message.payload).await?,
                    WriteOp::Untrack => self.untrack(&target, &message.payload)?,
                };
                self.forward(target.key(), reply, command).await
            }
            Route::Holder(op) => {
                let key = op
                    .parse(&message.subject)
                    .map_err(|err| Response::from(ErrorReply::new(err.code()).detail(err)))?;
                let command = match op {
                    HolderOp::Heartbeat => heartbeat_command(&message.payload)?,
                    HolderOp::Release => self.release_command(&message.payload)?,
                };
                self.forward(&key, reply, command).await
            }
            Route::Redirect(_) | Route::SnapshotRedirect | Route::HeartbeatMany => {
                Err(ErrorReply::new(ReplyCode::InvalidRequest).into())
            }
        }
    }

    fn identity(&self, request: Option<RequestIdentity>) -> Result<RequestIdentity, Response> {
        match request {
            Some(request) => Ok(request),
            None => RequestIdentity::mint(self.inner.window).map_err(unavailable),
        }
    }

    async fn enrich(&self, op: HookOp, target: &WriteTarget, meta: &Meta) -> Result<Option<Meta>, Response> {
        let Some(hook) = &self.inner.hook else {
            return Ok(None);
        };
        match hook.enrich(op, target.topic(), target.key(), meta).await {
            HookOutcome::Enriched(meta) => Ok(Some(meta)),
            HookOutcome::Rejected(reason) => Err(ErrorReply::new(ReplyCode::HookRejected).detail(reason).into()),
            HookOutcome::Unavailable(failure) => {
                Err(ErrorReply::new(ReplyCode::HookUnavailable).detail(failure).into())
            }
        }
    }

    async fn track(&self, target: &WriteTarget, payload: &[u8]) -> Result<Command, Response> {
        let body: TrackBody = parse_required(payload)?;
        check_body(target, body.topic.as_ref(), body.key.as_ref())?;
        let enriched = self.enrich(HookOp::Track, target, &body.meta).await?;
        Ok(Command::Track {
            topic: target.topic().clone(),
            request: self.identity(body.request)?,
            holder: body.holder,
            meta: body.meta,
            enriched,
        })
    }

    async fn update(&self, target: &WriteTarget, payload: &[u8]) -> Result<Command, Response> {
        let body: UpdateBody = parse_required(payload)?;
        check_body(target, body.topic.as_ref(), body.key.as_ref())?;
        let enriched = self.enrich(HookOp::Update, target, &body.meta).await?;
        Ok(Command::Update {
            topic: target.topic().clone(),
            request: self.identity(body.request)?,
            holder: body.holder,
            meta: body.meta,
            enriched,
            target: EntryRef::new(body.lifetime, body.mutation_seq),
            expected_ref: body.expected_ref,
        })
    }

    fn untrack(&self, target: &WriteTarget, payload: &[u8]) -> Result<Command, Response> {
        let body: UntrackBody = parse_required(payload)?;
        check_body(target, body.topic.as_ref(), body.key.as_ref())?;
        Ok(Command::Untrack {
            topic: target.topic().clone(),
            request: self.identity(body.request)?,
            holder: body.holder,
            target: EntryRef::new(body.lifetime, body.mutation_seq),
        })
    }

    fn release_command(&self, payload: &[u8]) -> Result<Command, Response> {
        let body: ReleaseBody = parse_required(payload)?;
        if body.targets.len() > RELEASE_MAX_TARGETS {
            return Err(ErrorReply::new(ReplyCode::TopicLimit)
                .detail(format!("at most {RELEASE_MAX_TARGETS} topics per release"))
                .into());
        }
        Ok(Command::Release {
            request: self.identity(body.request)?,
            holder: body.holder,
            targets: body.targets,
        })
    }

    pub(crate) async fn envelope(
        &self,
        key: &PresenceKey,
        reply: Option<Subject>,
        command: Command,
    ) -> Result<(String, Vec<u8>), Response> {
        let inner = &self.inner;
        let shard = WriterShard::of(key, inner.shards);
        let holder = inner
            .leases
            .holder(LeaseKey::Writer(shard))
            .await
            .map_err(unavailable)?
            .ok_or_else(|| Response::from(ErrorReply::new(ReplyCode::NotReady).detail("writer shard has no owner")))?;
        if holder.value().generation() != inner.generation {
            return Err(ErrorReply::new(ReplyCode::GenerationChanged).into());
        }
        let subject = internal_write_subject(inner.shards, key, command.topic());
        let epoch = OwnerEpoch::new(holder.revision(), holder.value().owner());
        let envelope = Forwarded::new(key.clone(), epoch, inner.generation, reply, command);
        let payload = serde_json::to_vec(&envelope).map_err(unavailable)?;
        Ok((subject, payload))
    }

    async fn forward(&self, key: &PresenceKey, reply: Option<Subject>, command: Command) -> Result<(), Response> {
        let deadline = tokio::time::Instant::now() + self.inner.reply_deadline.get();
        let (subject, payload) = tokio::time::timeout_at(deadline, self.envelope(key, reply, command))
            .await
            .map_err(|_| unavailable("could not resolve the writer shard owner within the reply deadline"))??;
        match tokio::time::timeout_at(deadline, self.inner.client.request(subject, payload.into())).await {
            Ok(Ok(_)) => {
                self.inner.counters.bump(Counter::Forwarded);
                Ok(())
            }
            Ok(Err(err)) => Err(handoff_failed(&err)),
            Err(_) => Err(handoff_unconfirmed()),
        }
    }

    pub(crate) fn count(&self, counter: Counter) {
        self.inner.counters.bump(counter);
    }
}

fn heartbeat_command(payload: &[u8]) -> Result<Command, Response> {
    let body: HeartbeatBody = parse_required(payload)?;
    if body.entries.len() > HEARTBEAT_MAX_ENTRIES {
        return Err(ErrorReply::new(ReplyCode::InvalidRequest)
            .detail(format!("at most {HEARTBEAT_MAX_ENTRIES} entries per heartbeat"))
            .into());
    }
    Ok(Command::Heartbeat { entries: body.entries })
}

fn check_header(headers: Option<&HeaderMap>, topic: &Topic) -> Result<(), Response> {
    match headers.and_then(|headers| headers.get(HEADER_TOPIC)) {
        Some(value) if value.as_str() != topic.as_str() => Err(ErrorReply::new(ReplyCode::InvalidRequest)
            .detail("Presence-Topic header does not match the subject topic")
            .into()),
        _ => Ok(()),
    }
}

fn check_body(target: &WriteTarget, topic: Option<&Topic>, key: Option<&PresenceKey>) -> Result<(), Response> {
    if topic.is_some_and(|topic| topic != target.topic()) {
        return Err(ErrorReply::new(ReplyCode::InvalidRequest)
            .detail("body topic does not match the subject topic")
            .into());
    }
    if key.is_some_and(|key| key != target.key()) {
        return Err(ErrorReply::new(ReplyCode::InvalidRequest)
            .detail("body key does not match the caller key")
            .into());
    }
    Ok(())
}

fn handoff_failed(err: &RequestError) -> Response {
    match err.kind() {
        RequestErrorKind::NoResponders => ErrorReply::new(ReplyCode::NotReady)
            .detail("no writer is subscribed for the owning shard")
            .into(),
        RequestErrorKind::TimedOut => handoff_unconfirmed(),
        RequestErrorKind::InvalidSubject | RequestErrorKind::MaxPayloadExceeded => unavailable(err),
        RequestErrorKind::Other => ErrorReply::new(ReplyCode::Unavailable)
            .outcome_unknown()
            .detail(err)
            .into(),
    }
}

fn handoff_unconfirmed() -> Response {
    ErrorReply::new(ReplyCode::Unavailable)
        .outcome_unknown()
        .detail("the writer did not confirm the hand-off within the reply deadline")
        .into()
}

pub(crate) fn unavailable(err: impl std::fmt::Display) -> Response {
    ErrorReply::new(ReplyCode::Unavailable).detail(err).into()
}

fn overloaded(detail: &'static str) -> Response {
    ErrorReply::new(ReplyCode::Overloaded).detail(detail).into()
}

#[cfg(test)]
mod tests {
    use async_nats::{RequestError, RequestErrorKind};
    use serde_json::Value;

    use super::handoff_failed;

    fn reply(kind: RequestErrorKind) -> Value {
        serde_json::from_slice(handoff_failed(&RequestError::from(kind)).body()).expect("a json reply")
    }

    #[test]
    fn a_shard_without_a_subscribed_writer_is_not_ready_with_a_known_outcome() {
        let body = reply(RequestErrorKind::NoResponders);
        assert_eq!(body["error"], "not_ready");
        assert_eq!(body["outcome_unknown"], false);
    }

    #[test]
    fn an_unconfirmed_hand_off_is_unavailable_with_an_unknown_outcome() {
        for kind in [RequestErrorKind::TimedOut, RequestErrorKind::Other] {
            let body = reply(kind);
            assert_eq!(body["error"], "unavailable");
            assert_eq!(body["outcome_unknown"], true);
        }
    }
}
