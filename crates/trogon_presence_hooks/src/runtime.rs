use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::time::{timeout_at, Instant};
use tracing::Instrument;
use trogon_presence::{Meta, PresenceKey, Topic};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, ResourceLimiter, Store, StoreLimits,
    StoreLimitsBuilder,
};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::config::{HookConfig, HookPolicy};
use crate::http::{HttpsTransport, OutboundPolicy, TlsSetupError};
use crate::imports::{HttpCapability, ImportAllowlist, UnsupportedImport};
use crate::output::{OutputLimit, RawOutput};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "hooks",
        exports: { default: async },
    });
}

use bindings::{HookError, Hooks, HooksPre, Op};

const EPOCH_TICK: Duration = Duration::from_millis(5);
const RUNNING_CALLS: usize = 1;
const QUEUED_CALLS: usize = 1;
const ADMITTED_CALLS: usize = RUNNING_CALLS + QUEUED_CALLS;
const CORE_INSTANCES_PER_CALL: usize = 4;
const MEMORIES_PER_CALL: usize = 1;
const TABLES_PER_CALL: usize = 2;
const TABLE_ELEMENTS: usize = 65_536;
const HOST_RESOURCES_PER_CALL: usize = 128;
const WASM_STACK_BYTES: usize = 512 * 1024;
const ASYNC_STACK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookOp {
    Track,
    Update,
    Retrack,
}

impl HookOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Update => "update",
            Self::Retrack => "retrack",
        }
    }
}

impl From<HookOp> for Op {
    fn from(op: HookOp) -> Self {
        match op {
            HookOp::Track => Self::Track,
            HookOp::Update => Self::Update,
            HookOp::Retrack => Self::Retrack,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookOutcome {
    Enriched(Meta),
    Rejected(String),
    Unavailable(HookFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookFailure {
    #[error("hook exceeded its deadline")]
    Deadline,
    #[error("hook is overloaded")]
    Overloaded,
    #[error("hook exceeded its memory limit")]
    MemoryLimit,
    #[error("hook reported an error: {0}")]
    Guest(String),
    #[error("hook returned meta that is not a valid JSON object: {0}")]
    Malformed(String),
    #[error("hook returned {0}")]
    Oversized(OutputLimit),
    #[error("hook trapped: {0}")]
    Trap(String),
}

impl HookFailure {
    fn kind(&self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::Overloaded => "overloaded",
            Self::MemoryLimit => "memory_limit",
            Self::Guest(_) => "guest_error",
            Self::Malformed(_) => "malformed",
            Self::Oversized(_) => "too_large",
            Self::Trap(_) => "trap",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HookLoadError {
    #[error("could not read hook component {path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("could not configure the hook engine: {0}")]
    Engine(String),
    #[error(transparent)]
    Tls(#[from] TlsSetupError),
    #[error("could not compile hook component: {0}")]
    Compile(String),
    #[error(transparent)]
    Import(#[from] UnsupportedImport),
    #[error("hook component does not satisfy the trogon:presence/hooks@0.1.0 world: {0}")]
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ComponentDigest(String);

impl ComponentDigest {
    fn of(bytes: &[u8]) -> Self {
        let hash = Sha256::digest(bytes);
        Self(format!("sha256:{}", hex(&hash)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ComponentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookInvocation {
    digest: ComponentDigest,
    outcome: HookOutcome,
}

impl HookInvocation {
    pub fn digest(&self) -> &ComponentDigest {
        &self.digest
    }

    pub fn outcome(&self) -> &HookOutcome {
        &self.outcome
    }

    pub fn into_outcome(self) -> HookOutcome {
        self.outcome
    }
}

#[derive(Debug, Clone, Copy)]
struct InvocationDeadline(Instant);

type CallResult = Result<Result<Meta, String>, HookFailure>;

impl InvocationDeadline {
    fn starting_now(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    fn instant(self) -> Instant {
        self.0
    }

    fn expired(self) -> bool {
        Instant::now() >= self.0
    }

    fn ensure_open(self) -> Result<(), HookFailure> {
        if self.expired() {
            Err(HookFailure::Deadline)
        } else {
            Ok(())
        }
    }

    fn conclude(self, completed: Option<CallResult>) -> CallResult {
        match completed {
            Some(Ok(Err(reason))) => Ok(Err(reason)),
            Some(_) if self.expired() => Err(HookFailure::Deadline),
            Some(result) => result,
            None => Err(HookFailure::Deadline),
        }
    }
}

pub(crate) struct HostState {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    transport: HttpsTransport,
    table: ResourceTable,
    limits: InvocationLimits,
}

impl HostState {
    fn new(outbound: Arc<OutboundPolicy>, deadline: InvocationDeadline, memory_bytes: usize) -> Self {
        let mut table = ResourceTable::new();
        table.set_max_capacity(HOST_RESOURCES_PER_CALL);
        Self {
            wasi: WasiCtx::builder()
                .allow_tcp(false)
                .allow_udp(false)
                .allow_ip_name_lookup(false)
                .build(),
            http: WasiHttpCtx::new(),
            transport: HttpsTransport::new(outbound, deadline.instant()),
            table,
            limits: InvocationLimits {
                limits: StoreLimitsBuilder::new()
                    .memory_size(memory_bytes)
                    .memories(MEMORIES_PER_CALL)
                    .tables(TABLES_PER_CALL)
                    .table_elements(TABLE_ELEMENTS)
                    .instances(CORE_INSTANCES_PER_CALL)
                    .trap_on_grow_failure(true)
                    .build(),
                memory_exceeded: false,
            },
        }
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.transport,
        }
    }
}

struct InvocationLimits {
    limits: StoreLimits,
    memory_exceeded: bool,
}

impl ResourceLimiter for InvocationLimits {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let allowed = self.limits.memory_growing(current, desired, maximum);
        self.memory_exceeded |= !matches!(allowed, Ok(true));
        allowed
    }

    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

struct EpochTicker {
    stop: Arc<AtomicBool>,
}

impl EpochTicker {
    fn spawn(engine: Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::Builder::new()
            .name("presence-hook-epoch".to_owned())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .map(|_| ())
            .unwrap_or_else(|err| tracing::error!(%err, "could not start the hook epoch ticker"));
        Self { stop }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct ActiveComponent {
    pre: HooksPre<HostState>,
    digest: ComponentDigest,
}

struct Inner {
    engine: Engine,
    linker: Linker<HostState>,
    imports: ImportAllowlist,
    outbound: Arc<OutboundPolicy>,
    active: RwLock<Arc<ActiveComponent>>,
    config: HookConfig,
    admission: Arc<Semaphore>,
    running: Arc<Semaphore>,
    _ticker: EpochTicker,
}

#[derive(Clone)]
pub struct HookRuntime {
    inner: Arc<Inner>,
}

impl fmt::Debug for HookRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRuntime")
            .field("config", &self.inner.config)
            .field("digest", &self.digest())
            .finish_non_exhaustive()
    }
}

fn engine(config: &HookConfig) -> Result<Engine, HookLoadError> {
    let memory = config.memory_limit.bytes();
    let mut pool = PoolingAllocationConfig::new();
    pool.total_component_instances(ADMITTED_CALLS as u32)
        .total_core_instances((ADMITTED_CALLS * CORE_INSTANCES_PER_CALL) as u32)
        .total_memories((ADMITTED_CALLS * MEMORIES_PER_CALL) as u32)
        .total_tables((ADMITTED_CALLS * TABLES_PER_CALL) as u32)
        .total_stacks(ADMITTED_CALLS as u32)
        .max_core_instances_per_component(CORE_INSTANCES_PER_CALL as u32)
        .max_memories_per_component(MEMORIES_PER_CALL as u32)
        .max_tables_per_component(TABLES_PER_CALL as u32)
        .table_elements(TABLE_ELEMENTS)
        .max_memory_size(memory);
    let mut wasm = Config::new();
    wasm.wasm_component_model(true)
        .epoch_interruption(true)
        .max_wasm_stack(WASM_STACK_BYTES)
        .async_stack_size(ASYNC_STACK_BYTES)
        .allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
    Engine::new(&wasm).map_err(|err| HookLoadError::Engine(format!("{err:#}")))
}

fn linker(engine: &Engine, http: HttpCapability) -> Result<Linker<HostState>, HookLoadError> {
    let link_error = |err: wasmtime::Error| HookLoadError::Link(format!("{err:#}"));
    let mut linker: Linker<HostState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(link_error)?;
    if http == HttpCapability::Enabled {
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker).map_err(link_error)?;
    }
    Ok(linker)
}

fn read(path: &Path) -> Result<Vec<u8>, HookLoadError> {
    std::fs::read(path).map_err(|source| HookLoadError::Read {
        path: path.display().to_string(),
        source,
    })
}

impl HookRuntime {
    pub fn load(config: HookConfig) -> Result<Self, HookLoadError> {
        let bytes = read(&config.component_path)?;
        let http = if config.http_allow.is_empty() {
            HttpCapability::Disabled
        } else {
            HttpCapability::Enabled
        };
        let engine = engine(&config)?;
        let linker = linker(&engine, http)?;
        let imports = ImportAllowlist::new(http);
        let active = prepare(&engine, &linker, imports, &bytes)?;
        let outbound = Arc::new(OutboundPolicy::new(&config.http_allow, &config.https_trust)?);
        tracing::info!(
            gauge.trogon.presence.hook.component.info = 1u64,
            presence.hook.component.digest = %active.digest,
            presence.hook.policy = %config.policy,
            presence.hook.http = %http,
            path = %config.component_path.display(),
            "hook component loaded"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                _ticker: EpochTicker::spawn(engine.clone()),
                engine,
                linker,
                imports,
                outbound,
                active: RwLock::new(Arc::new(active)),
                config,
                admission: Arc::new(Semaphore::new(ADMITTED_CALLS)),
                running: Arc::new(Semaphore::new(RUNNING_CALLS)),
            }),
        })
    }

    pub fn reload(&self, candidate: &Path) -> Result<ComponentDigest, HookLoadError> {
        let prepared = read(candidate)
            .and_then(|bytes| prepare(&self.inner.engine, &self.inner.linker, self.inner.imports, &bytes));
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                tracing::warn!(
                    %err,
                    presence.hook.component.digest = %self.digest(),
                    path = %candidate.display(),
                    "hook candidate rejected, keeping the active component"
                );
                return Err(err);
            }
        };
        let digest = prepared.digest.clone();
        *self.inner.active.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(prepared);
        tracing::info!(
            gauge.trogon.presence.hook.component.info = 1u64,
            presence.hook.component.digest = %digest,
            path = %candidate.display(),
            "hook component reloaded"
        );
        Ok(digest)
    }

    pub fn digest(&self) -> ComponentDigest {
        self.active().digest.clone()
    }

    pub fn config(&self) -> &HookConfig {
        &self.inner.config
    }

    fn active(&self) -> Arc<ActiveComponent> {
        self.inner.active.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub async fn enrich(&self, op: HookOp, topic: &Topic, key: &PresenceKey, meta: &Meta) -> HookOutcome {
        self.invoke(op, topic, key, meta).await.into_outcome()
    }

    pub async fn invoke(&self, op: HookOp, topic: &Topic, key: &PresenceKey, meta: &Meta) -> HookInvocation {
        let deadline = InvocationDeadline::starting_now(self.inner.config.deadline.get());
        let component = self.active();
        let span = tracing::info_span!(
            "presence.hook.enrich",
            presence.hook.op = op.as_str(),
            presence.hook.component.digest = %component.digest,
            presence.hook.outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        let digest = component.digest.clone();
        async {
            let result = match self.inner.admission.clone().try_acquire_owned() {
                Ok(_admitted) => {
                    let completed = timeout_at(
                        deadline.instant(),
                        self.call(&component, op, topic, key, meta, deadline),
                    )
                    .await
                    .ok();
                    deadline.conclude(completed)
                }
                Err(_) => Err(HookFailure::Overloaded),
            };
            let outcome = self.settle(result, meta);
            tracing::Span::current().record("presence.hook.outcome", outcome_label(&outcome));
            tracing::debug!(
                histogram.trogon.presence.hook.duration = self.inner.config.deadline.get().as_secs_f64()
                    - deadline
                        .instant()
                        .saturating_duration_since(Instant::now())
                        .as_secs_f64(),
                presence.hook.op = op.as_str(),
                presence.hook.outcome = outcome_label(&outcome),
                "hook call finished"
            );
            HookInvocation { digest, outcome }
        }
        .instrument(span)
        .await
    }

    fn settle(&self, result: CallResult, original: &Meta) -> HookOutcome {
        match result {
            Ok(Ok(meta)) => HookOutcome::Enriched(meta),
            Ok(Err(reason)) => HookOutcome::Rejected(reason),
            Err(failure) => {
                tracing::Span::current().record("error.type", failure.kind());
                match self.inner.config.policy {
                    HookPolicy::FailOpen => {
                        tracing::warn!(%failure, "hook unavailable, admitting the original meta under fail-open");
                        HookOutcome::Enriched(original.clone())
                    }
                    HookPolicy::FailClosed => {
                        tracing::warn!(%failure, "hook unavailable, refusing the write under fail-closed");
                        HookOutcome::Unavailable(failure)
                    }
                }
            }
        }
    }

    async fn call(
        &self,
        component: &ActiveComponent,
        op: HookOp,
        topic: &Topic,
        key: &PresenceKey,
        meta: &Meta,
        deadline: InvocationDeadline,
    ) -> CallResult {
        let _running = self
            .inner
            .running
            .acquire()
            .await
            .map_err(|err| HookFailure::Trap(err.to_string()))?;
        deadline.ensure_open()?;
        let input = serde_json::to_vec(meta).map_err(|err| HookFailure::Malformed(err.to_string()))?;
        let mut store = self.store(deadline);
        let hooks: Hooks = match component.pre.instantiate_async(&mut store).await {
            Ok(hooks) => hooks,
            Err(err) => return Err(classify(store.data(), &err)),
        };
        deadline.ensure_open()?;
        let returned = match hooks
            .call_enrich(&mut store, op.into(), topic.as_str(), key.as_str(), &input)
            .await
        {
            Ok(returned) => returned,
            Err(err) => return Err(classify(store.data(), &err)),
        };
        match returned {
            Ok(bytes) => RawOutput::new(bytes)?.into_meta().map(Ok),
            Err(HookError::Reject(reason)) => Ok(Err(reason)),
            Err(HookError::Error(reason)) => Err(HookFailure::Guest(reason)),
        }
    }

    fn store(&self, deadline: InvocationDeadline) -> Store<HostState> {
        let state = HostState::new(
            self.inner.outbound.clone(),
            deadline,
            self.inner.config.memory_limit.bytes(),
        );
        let mut store = Store::new(&self.inner.engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        store
    }
}

fn prepare(
    engine: &Engine,
    linker: &Linker<HostState>,
    imports: ImportAllowlist,
    bytes: &[u8],
) -> Result<ActiveComponent, HookLoadError> {
    let digest = ComponentDigest::of(bytes);
    let component = Component::new(engine, bytes).map_err(|err| HookLoadError::Compile(format!("{err:#}")))?;
    imports.check(&component, engine)?;
    let pre = linker
        .instantiate_pre(&component)
        .and_then(HooksPre::new)
        .map_err(|err| HookLoadError::Link(format!("{err:#}")))?;
    Ok(ActiveComponent { pre, digest })
}

fn classify(state: &HostState, err: &wasmtime::Error) -> HookFailure {
    if state.limits.memory_exceeded {
        return HookFailure::MemoryLimit;
    }
    if matches!(err.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt)) {
        return HookFailure::Deadline;
    }
    HookFailure::Trap(format!("{err:#}"))
}

fn outcome_label(outcome: &HookOutcome) -> &'static str {
    match outcome {
        HookOutcome::Enriched(_) => "enriched",
        HookOutcome::Rejected(_) => "rejected",
        HookOutcome::Unavailable(_) => "unavailable",
    }
}

#[cfg(test)]
#[path = "../tests/support/guest.rs"]
mod guest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TrustAnchors;

    fn enriched() -> Meta {
        serde_json::from_str(r#"{"enriched":true}"#).expect("valid meta")
    }

    #[tokio::test(start_paused = true)]
    async fn late_success_is_rejected_after_the_deadline() {
        let deadline = InvocationDeadline::starting_now(Duration::from_millis(200));
        tokio::time::advance(Duration::from_millis(199)).await;
        assert_eq!(deadline.conclude(Some(Ok(Ok(enriched())))), Ok(Ok(enriched())));
        tokio::time::advance(Duration::from_millis(2)).await;
        assert_eq!(deadline.conclude(Some(Ok(Ok(enriched())))), Err(HookFailure::Deadline));
        assert_eq!(
            deadline.conclude(Some(Err(HookFailure::Guest("late".to_owned())))),
            Err(HookFailure::Deadline)
        );
        assert_eq!(deadline.ensure_open(), Err(HookFailure::Deadline));
        assert_eq!(deadline.conclude(None), Err(HookFailure::Deadline));
    }

    #[tokio::test(start_paused = true)]
    async fn late_reject_is_still_honored() {
        let deadline = InvocationDeadline::starting_now(Duration::from_millis(200));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(
            deadline.conclude(Some(Ok(Err("blocked".to_owned())))),
            Ok(Err("blocked".to_owned()))
        );
    }

    fn live_stores(runtime: &HookRuntime) -> usize {
        Arc::strong_count(&runtime.inner.outbound) - 1
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_an_in_flight_call_drops_its_store_and_permits() {
        let Some(path) = guest::example_component() else {
            return;
        };
        let runtime = HookRuntime::load(HookConfig {
            deadline: crate::config::HookDeadline::try_from(Duration::from_secs(5)).expect("deadline"),
            ..HookConfig::new(path)
        })
        .expect("runtime");
        let topic: Topic = "room:lobby".parse().expect("topic");
        let key: PresenceKey = "slow".parse().expect("key");
        let meta: Meta = serde_json::from_str(r#"{"status":"online"}"#).expect("meta");

        let mut running = Box::pin(runtime.invoke(HookOp::Track, &topic, &key, &meta));
        let mut queued = Box::pin(runtime.invoke(HookOp::Track, &topic, &key, &meta));
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut running)
            .await
            .is_err());
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut queued)
            .await
            .is_err());
        assert_eq!(live_stores(&runtime), 1, "only the running call owns a store");
        assert_eq!(runtime.inner.running.available_permits(), 0);
        assert_eq!(runtime.inner.admission.available_permits(), 0);

        drop(queued);
        assert_eq!(runtime.inner.admission.available_permits(), 1);
        assert_eq!(runtime.inner.running.available_permits(), 0);

        drop(running);
        assert_eq!(live_stores(&runtime), 0, "the guest store outlived its caller");
        assert_eq!(runtime.inner.running.available_permits(), RUNNING_CALLS);
        assert_eq!(runtime.inner.admission.available_permits(), ADMITTED_CALLS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_a_call_blocked_on_http_drops_its_store() {
        use tokio::io::AsyncReadExt;
        let Some(path) = guest::http_probe_component() else {
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().expect("local addr").port();
        let (accepted_tx, accepted) = tokio::sync::oneshot::channel();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let _ = accepted_tx.send(());
            let mut buffer = [0u8; 1024];
            while let Ok(read) = tcp.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
            }
            let _ = closed_tx.send(Instant::now());
        });
        let runtime = HookRuntime::load(HookConfig {
            deadline: crate::config::HookDeadline::try_from(Duration::from_secs(5)).expect("deadline"),
            http_allow: vec![format!("127.0.0.1:{port}").parse().expect("allowed host")],
            ..HookConfig::new(path)
        })
        .expect("runtime");
        let topic: Topic = port.to_string().parse().expect("topic");
        let key: PresenceKey = "probe".parse().expect("key");
        let meta: Meta = serde_json::from_str("{}").expect("meta");
        let mut call = Box::pin(runtime.invoke(HookOp::Track, &topic, &key, &meta));
        tokio::select! {
            finished = &mut call => panic!("probe finished: {finished:?}"),
            seen = accepted => seen.expect("accepted"),
        }
        assert_eq!(live_stores(&runtime), 1);
        let dropped = Instant::now();
        drop(call);
        assert_eq!(live_stores(&runtime), 0, "the store outlived its caller");
        let closed = tokio::time::timeout(Duration::from_secs(1), closed)
            .await
            .expect("the connection task outlived the store")
            .expect("server");
        assert!(closed.saturating_duration_since(dropped) < Duration::from_millis(100));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sockets_are_denied_inside_the_guest() {
        let Some(path) = guest::socket_probe_component() else {
            return;
        };
        let config = HookConfig::new(&path);
        let engine = engine(&config).expect("engine");
        let _ticker = EpochTicker::spawn(engine.clone());
        let mut linker: Linker<HostState> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).expect("full wasi linker");
        let component = Component::from_file(&engine, &path).expect("socket probe compiles");
        let pre = HooksPre::new(linker.instantiate_pre(&component).expect("probe links")).expect("hooks world");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().expect("local addr").port().to_string();
        let outbound = Arc::new(OutboundPolicy::new(&[], &TrustAnchors::default()).expect("outbound policy"));
        let deadline = InvocationDeadline::starting_now(Duration::from_secs(5));
        let mut store = Store::new(&engine, HostState::new(outbound, deadline, config.memory_limit.bytes()));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        let hooks = pre.instantiate_async(&mut store).await.expect("probe instantiates");
        let returned = hooks
            .call_enrich(&mut store, Op::Track, &port, "probe", b"{}")
            .await
            .expect("probe runs");

        let Err(HookError::Error(report)) = returned else {
            panic!("socket probe returned {returned:?}");
        };
        assert!(report.contains("lookup=Err"), "{report}");
        assert!(report.contains("connect=Err"), "{report}");
        let accepted = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(accepted.is_err(), "the guest reached the host listener");
    }
}
