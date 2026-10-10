#[path = "support/guest.rs"]
mod guest;

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use trogon_presence::{Meta, PresenceKey, Topic};
use trogon_presence_hooks::{
    AllowedHost, HookConfig, HookDeadline, HookFailure, HookInvocation, HookOp, HookOutcome, HookPolicy, HookRuntime,
    TrustAnchors,
};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const DEADLINE: Duration = Duration::from_secs(2);
const RUNNING: Duration = Duration::from_millis(50);
const PROMPT: Duration = Duration::from_millis(100);

fn config(path: std::path::PathBuf) -> Result<HookConfig, Box<dyn Error + Send + Sync>> {
    Ok(HookConfig {
        policy: HookPolicy::FailClosed,
        deadline: HookDeadline::try_from(DEADLINE)?,
        ..HookConfig::new(path)
    })
}

fn meta() -> Result<Meta, serde_json::Error> {
    serde_json::from_value(json!({ "status": "online" }))
}

async fn invoke(runtime: &HookRuntime, topic: &str, key: &str) -> Result<HookInvocation, Box<dyn Error + Send + Sync>> {
    let topic: Topic = topic.parse()?;
    let key: PresenceKey = key.parse()?;
    Ok(runtime.invoke(HookOp::Track, &topic, &key, &meta()?).await)
}

async fn enrich(runtime: &HookRuntime, key: &str) -> Result<HookOutcome, Box<dyn Error + Send + Sync>> {
    Ok(invoke(runtime, "room:lobby", key).await?.into_outcome())
}

fn spawn_invoke(runtime: &HookRuntime, key: &'static str) -> tokio::task::JoinHandle<Result<HookInvocation, String>> {
    let runtime = runtime.clone();
    tokio::spawn(async move { invoke(&runtime, "room:lobby", key).await.map_err(|err| err.to_string()) })
}

fn first_instance() -> Result<HookOutcome, serde_json::Error> {
    Ok(HookOutcome::Enriched(serde_json::from_value(json!({ "calls": 1 }))?))
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_running_call_admits_the_next_call_promptly() -> TestResult {
    let Some(path) = guest::example_component() else {
        return Ok(());
    };
    let runtime = HookRuntime::load(config(path)?)?;
    tokio::select! {
        finished = invoke(&runtime, "room:lobby", "slow") => panic!("the slow guest finished: {:?}", finished?),
        () = tokio::time::sleep(RUNNING) => {}
    }
    let cancelled = Instant::now();
    assert_eq!(enrich(&runtime, "fresh").await?, first_instance()?);
    let latency = cancelled.elapsed();
    eprintln!("cancel running: next call completed {latency:?} after the drop");
    assert!(latency < PROMPT, "next call took {latency:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn aborting_a_running_task_frees_its_permit() -> TestResult {
    let Some(path) = guest::example_component() else {
        return Ok(());
    };
    let runtime = HookRuntime::load(config(path)?)?;
    let running = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(RUNNING).await;
    let aborted = Instant::now();
    running.abort();
    assert!(running.await.is_err_and(|err| err.is_cancelled()));
    assert_eq!(enrich(&runtime, "fresh").await?, first_instance()?);
    let latency = aborted.elapsed();
    eprintln!("abort running: next call completed {latency:?} after the abort");
    assert!(latency < PROMPT, "next call took {latency:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_queued_call_frees_the_queue_slot() -> TestResult {
    let Some(path) = guest::example_component() else {
        return Ok(());
    };
    let runtime = HookRuntime::load(config(path)?)?;
    let running = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let queued = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        enrich(&runtime, "ana").await?,
        HookOutcome::Unavailable(HookFailure::Overloaded)
    );

    let aborted = Instant::now();
    queued.abort();
    assert!(queued.await.is_err_and(|err| err.is_cancelled()));
    let freed = aborted.elapsed();
    let waiting = tokio::time::timeout(RUNNING, enrich(&runtime, "ana")).await;
    assert!(
        waiting.is_err(),
        "the freed slot must admit a call that then waits: {waiting:?}"
    );
    eprintln!("cancel queued: slot free {freed:?} after the abort");
    assert!(freed < PROMPT, "queue slot took {freed:?}");

    tokio::select! {
        finished = invoke(&runtime, "room:lobby", "ana") => panic!("the queued call ran ahead of the slow one: {:?}", finished?),
        () = tokio::time::sleep(RUNNING) => {}
    }
    running.abort();
    assert!(running.await.is_err_and(|err| err.is_cancelled()));
    assert_eq!(enrich(&runtime, "fresh").await?, first_instance()?);
    Ok(())
}

struct HangServer {
    port: u16,
    certificate: CertificateDer<'static>,
    request_seen: oneshot::Receiver<()>,
    closed: oneshot::Receiver<Instant>,
}

impl HangServer {
    async fn start() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
        let certificate = generated.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()));
        let tls = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)?;
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (seen_tx, request_seen) = oneshot::channel();
        let (closed_tx, closed) = oneshot::channel();
        tokio::spawn(async move {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut stream) = acceptor.accept(tcp).await else {
                return;
            };
            let mut head = Vec::new();
            let mut buffer = [0u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => head.extend_from_slice(&buffer[..read]),
                }
            }
            let _ = seen_tx.send(());
            while let Ok(read) = stream.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
            }
            let _ = closed_tx.send(Instant::now());
        });
        Ok(Self {
            port,
            certificate,
            request_seen,
            closed,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_call_during_http_closes_its_connection() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let server = HangServer::start().await?;
    let runtime = HookRuntime::load(HookConfig {
        http_allow: vec![format!("127.0.0.1:{}", server.port).parse::<AllowedHost>()?],
        https_trust: TrustAnchors::Pinned(vec![server.certificate.clone()]),
        ..config(path)?
    })?;
    let port = server.port.to_string();
    let mut call = Box::pin(invoke(&runtime, &port, "probe"));
    tokio::select! {
        finished = &mut call => panic!("the probe finished before the server answered: {:?}", finished?),
        seen = tokio::time::timeout(DEADLINE / 2, server.request_seen) => seen.map_err(|_| "the request never reached the server")??,
    }
    let in_flight = tokio::time::timeout(RUNNING, &mut call).await;
    assert!(in_flight.is_err(), "the probe must still wait on the hanging server");

    let dropped = Instant::now();
    drop(call);
    let closed = tokio::time::timeout(PROMPT, server.closed)
        .await
        .map_err(|_| "the connection stayed open after the drop")??;
    let latency = closed.saturating_duration_since(dropped);
    eprintln!("cancel during http: server saw the connection close {latency:?} after the drop");
    assert!(latency < PROMPT, "connection closed after {latency:?}");
    Ok(())
}
