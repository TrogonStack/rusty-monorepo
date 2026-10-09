use std::collections::HashMap;
use std::sync::{Arc, PoisonError};

use tokio::sync::Semaphore;

use async_nats::jetstream::context::GetStreamError;
use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use trogon_presence::watch::engine::{RetirementWindow, StalePolicy};
use trogon_presence::{
    ClockError, EntropyError, OpenError, OwnerId, Presence, ProvisionError, ProvisionOptions, SelfFence,
    SuspendAwareClock, ViewShard, WriterShard,
};
use trogon_presence_hooks::{HookLoadError, HookRuntime};

use crate::admission::{AdmissionCounters, AdmissionStats};
use crate::config::ServiceConfig;
use crate::lease::{provision_lease_bucket, LeaseError, LeaseKey, LeaseStore, LeaseValue};
use crate::shard_owner::{OwnedShards, OwnerExit, OwnerShared, ShardOwner};
use crate::snapshot::AssemblyGate;
use crate::writer::{WriterContext, WriterHost, WriterShards};
use crate::writes::{Ingress, Writes};

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Provision(#[from] ProvisionError),
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error("presence stream lookup failed: {0}")]
    Stream(#[from] GetStreamError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error(transparent)]
    Hook(#[from] HookLoadError),
    #[error(transparent)]
    Clock(#[from] ClockError),
}

pub async fn provision(
    client: async_nats::Client,
    config: &ServiceConfig,
    options: ProvisionOptions,
) -> Result<(), ServiceError> {
    Presence::provision(client.clone(), config.presence().clone(), options.clone())
        .await?
        .close();
    provision_lease_bucket(
        client,
        config.presence().route(),
        config.lease_bucket(),
        config.lease_ttl(),
        options,
    )
    .await?;
    Ok(())
}

pub struct ServiceHandle {
    owned: OwnedShards,
    writers: WriterShards,
    counters: Arc<AdmissionCounters>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ServiceHandle {
    pub fn owned_shards(&self) -> Vec<ViewShard> {
        self.owned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    pub fn writer_shards(&self) -> Vec<WriterShard> {
        self.writers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    pub fn stats(&self) -> AdmissionStats {
        self.counters.snapshot()
    }

    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        if let Err(err) = self.task.await {
            tracing::warn!(%err, "presence service task failed");
        }
    }
}

/// Starts the service on the platform suspend-aware clock; platforms without one are refused.
pub async fn start(client: async_nats::Client, config: ServiceConfig) -> Result<ServiceHandle, ServiceError> {
    start_with_clock(client, config, SuspendAwareClock::system()?).await
}

/// Starts the service with an explicit owner clock.
pub async fn start_with_clock(
    client: async_nats::Client,
    config: ServiceConfig,
    clock: SuspendAwareClock,
) -> Result<ServiceHandle, ServiceError> {
    let hook = match config.hook() {
        Some(hook) => Some(HookRuntime::load(hook.clone())?),
        None => {
            tracing::warn!("no hook component configured, meta is stored as clients send it and is untrusted");
            None
        }
    };
    let presence = Presence::open(client.clone(), config.presence().clone()).await?;
    let stream = config
        .presence()
        .context(client.clone())
        .get_stream(config.presence().bucket().stream_name())
        .await?;
    let shards = config.presence().shards();
    let owner = OwnerId::generate()?;
    let generation = presence.generation();
    let leases = LeaseStore::open_routed(
        client.clone(),
        config.presence().route().clone(),
        config.lease_bucket().clone(),
        shards,
        config.lease_ttl(),
        LeaseValue::new(owner, generation),
    )
    .await?;
    let owned = OwnedShards::default();
    let writers = WriterShards::default();
    let counters = Arc::new(AdmissionCounters::default());
    let admission = config.admission();
    let host = WriterHost::new(WriterContext {
        client: client.clone(),
        presence: presence.clone(),
        leases: leases.clone(),
        owner,
        generation,
        shards,
        limits: config.managed_limits(),
        key_depth: admission.key(),
        permits: Arc::new(Semaphore::new(admission.process().get())),
        counters: counters.clone(),
        budget: config.rebuild_budget(),
        reply_deadline: config.writer_reply_deadline(),
        held: writers.clone(),
        clock: clock.clone(),
    });
    let limits = config.snapshot_limits();
    let snapshot_limits = limits.with_payload(limits.payload().for_server(&client.server_info()));
    let shared = Arc::new(OwnerShared {
        client: client.clone(),
        stream,
        classifier_bucket: config.presence().bucket().clone(),
        shards,
        leases,
        owned: owned.clone(),
        snapshot: snapshot_limits,
        gate: AssemblyGate::new(snapshot_limits),
        counters: counters.clone(),
        keepalive: config.keepalive(),
        coalesce: config.coalesce().get(),
        stale: StalePolicy::from(config.presence()),
        retirement: RetirementWindow::from(config.presence()),
        rebuild_budget: config.rebuild_budget(),
        reconcile: config.reconcile(),
        owner,
        generation,
        clock,
    });
    let (stop, stop_rx) = watch::channel(false);
    let writes = Writes::new(Ingress {
        client,
        presence: presence.clone(),
        leases: shared.leases.clone(),
        hook,
        counters: counters.clone(),
        limits: admission,
        reply_deadline: config.writer_reply_deadline(),
    });
    let front = tokio::spawn(writes.run(stop_rx.clone()));
    let task = tokio::spawn(manage(shared, host, presence, front, stop_rx));
    Ok(ServiceHandle {
        owned,
        writers,
        counters,
        stop,
        task,
    })
}

type Front = JoinHandle<Result<(), async_nats::SubscribeError>>;

async fn manage(
    shared: Arc<OwnerShared>,
    mut host: WriterHost,
    presence: Presence,
    front: Front,
    mut stop: watch::Receiver<bool>,
) {
    let mut owners: HashMap<ViewShard, JoinHandle<OwnerExit>> = HashMap::new();
    let mut tick = tokio::time::interval(shared.leases.ttl().renew_every());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = tick.tick() => {
                host.acquire_free().await;
                owners.retain(|_, owner| !owner.is_finished());
                let free: Vec<ViewShard> = (0..shared.shards.get())
                    .filter_map(|index| shared.shards.shard(index).ok().map(ViewShard::from))
                    .filter(|shard| !owners.contains_key(shard))
                    .collect();
                let attempts = free.into_iter().map(|shard| {
                    let leases = shared.leases.clone();
                    let sent = shared.clock.now();
                    async move { (shard, sent, leases.acquire(LeaseKey::View(shard)).await) }
                });
                for (shard, sent, outcome) in join_all(attempts).await {
                    match outcome {
                        Ok(Some(revision)) => {
                            let bound = shared.leases.ttl().self_fence();
                            match SelfFence::confirm(sent, shared.clock.now(), bound) {
                                Ok(fence) => {
                                    let owner = ShardOwner::new(shared.clone(), shard, revision, fence);
                                    owners.insert(shard, tokio::spawn(owner.run(stop.clone())));
                                }
                                Err(breach) => {
                                    tracing::warn!(%breach, "shard lease acquired too late, releasing it");
                                    if let Err(err) = shared.leases.release(LeaseKey::View(shard), revision).await {
                                        tracing::warn!(%err, "shard lease release failed");
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => tracing::warn!(%err, "shard lease acquire failed"),
                    }
                }
            }
        }
    }
    host.shutdown().await;
    for owner in owners.into_values() {
        if let Err(err) = owner.await {
            tracing::warn!(%err, "shard owner task failed");
        }
    }
    match front.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(%err, "front subscriptions failed"),
        Err(err) => tracing::warn!(%err, "front task failed"),
    }
    presence.close();
}
