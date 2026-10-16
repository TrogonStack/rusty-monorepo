#[path = "support/guest.rs"]
mod guest;

use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use trogon_presence::{Meta, PresenceKey, Topic};
use trogon_presence_hooks::{
    AllowedHost, HookConfig, HookDeadline, HookFailure, HookOp, HookOutcome, HookPolicy, HookRuntime, TrustAnchors,
};

type BoxError = Box<dyn Error + Send + Sync>;
type TestResult = Result<(), BoxError>;

const DEADLINE: Duration = Duration::from_secs(2);
const SHORT_DEADLINE: Duration = Duration::from_millis(500);
const PROMPT: Duration = Duration::from_millis(200);
const BODY_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone)]
enum Reply {
    Respond(Vec<u8>),
    Hang,
}

impl Reply {
    fn ok(body: &[u8]) -> Self {
        let mut bytes = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        Self::Respond(bytes)
    }

    fn redirect(location: &str) -> Self {
        Self::Respond(format!("HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n").into_bytes())
    }

    fn streamed(len: usize) -> Self {
        let mut bytes = format!("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{len:x}\r\n").into_bytes();
        bytes.extend(std::iter::repeat_n(b'x', len));
        bytes.extend_from_slice(b"\r\n0\r\n\r\n");
        Self::Respond(bytes)
    }

    fn declared(len: usize) -> Self {
        Self::Respond(format!("HTTP/1.1 200 OK\r\ncontent-length: {len}\r\n\r\n").into_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestHead(String);

impl RequestHead {
    fn request_line(&self) -> &str {
        self.0.lines().next().unwrap_or_default()
    }

    fn has_header(&self, name: &str, value: &str) -> bool {
        self.0.lines().skip(1).any(|line| {
            line.split_once(':').is_some_and(|(found, content)| {
                found.trim().eq_ignore_ascii_case(name) && content.trim().eq_ignore_ascii_case(value)
            })
        })
    }
}

struct ScriptedServer {
    port: u16,
    certificate: CertificateDer<'static>,
    connections: Arc<AtomicUsize>,
    heads: mpsc::UnboundedReceiver<RequestHead>,
    closed: mpsc::UnboundedReceiver<Instant>,
}

impl ScriptedServer {
    async fn start(reply: Reply) -> Result<Self, BoxError> {
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])?;
        let certificate = generated.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()));
        let tls = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)?;
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let connections = Arc::new(AtomicUsize::new(0));
        let (heads_tx, heads) = mpsc::unbounded_channel();
        let (closed_tx, closed) = mpsc::unbounded_channel();
        let counted = connections.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                let reply = reply.clone();
                let heads_tx = heads_tx.clone();
                let closed_tx = closed_tx.clone();
                tokio::spawn(async move {
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
                    let _ = heads_tx.send(RequestHead(String::from_utf8_lossy(&head).into_owned()));
                    match reply {
                        Reply::Respond(bytes) => {
                            let _ = stream.write_all(&bytes).await;
                            let _ = stream.flush().await;
                        }
                        Reply::Hang => {
                            while let Ok(read) = stream.read(&mut buffer).await {
                                if read == 0 {
                                    break;
                                }
                            }
                            let _ = closed_tx.send(Instant::now());
                        }
                    }
                });
            }
        });
        Ok(Self {
            port,
            certificate,
            connections,
            heads,
            closed,
        })
    }

    fn authority(&self, host: &str) -> String {
        format!("{host}:{}", self.port)
    }

    fn allowed(&self, host: &str) -> Result<AllowedHost, BoxError> {
        Ok(self.authority(host).parse()?)
    }

    fn trust(&self) -> TrustAnchors {
        TrustAnchors::Pinned(vec![self.certificate.clone()])
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

fn runtime(
    path: std::path::PathBuf,
    allow: Vec<AllowedHost>,
    trust: TrustAnchors,
    deadline: Duration,
) -> Result<HookRuntime, BoxError> {
    Ok(HookRuntime::load(HookConfig {
        policy: HookPolicy::FailClosed,
        deadline: HookDeadline::try_from(deadline)?,
        http_allow: allow,
        https_trust: trust,
        ..HookConfig::new(path)
    })?)
}

async fn probe(runtime: &HookRuntime, authority: &str, path: &str) -> Result<HookOutcome, BoxError> {
    let topic: Topic = authority.parse()?;
    let key: PresenceKey = path.parse()?;
    let meta: Meta = serde_json::from_value(json!({ "status": "online" }))?;
    Ok(runtime.enrich(HookOp::Track, &topic, &key, &meta).await)
}

fn guest_failure(outcome: HookOutcome) -> Result<String, BoxError> {
    match outcome {
        HookOutcome::Unavailable(HookFailure::Guest(reason)) => Ok(reason),
        other => Err(format!("expected the guest to report a refused request, got {other:?}").into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allowed_host_answers_the_guest() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let mut server = ScriptedServer::start(Reply::ok(br#"{"enriched":"remote"}"#)).await?;
    let runtime = runtime(path, vec![server.allowed("127.0.0.1")?], server.trust(), DEADLINE)?;

    let outcome = probe(&runtime, &server.authority("127.0.0.1"), "ana").await?;

    let expected: Meta = serde_json::from_value(json!({ "enriched": "remote" }))?;
    assert_eq!(outcome, HookOutcome::Enriched(expected));
    let head = server.heads.try_recv()?;
    assert_eq!(head.request_line(), "GET /ana HTTP/1.1");
    assert!(head.has_header("host", &server.authority("127.0.0.1")), "{head:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused_before_connecting() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let allowed = ScriptedServer::start(Reply::ok(b"{}")).await?;
    let outsider = ScriptedServer::start(Reply::ok(b"{}")).await?;
    let runtime = runtime(path, vec![allowed.allowed("127.0.0.1")?], allowed.trust(), DEADLINE)?;

    for authority in [outsider.authority("127.0.0.1"), "api.example.com".to_owned()] {
        let reason = guest_failure(probe(&runtime, &authority, "ana").await?)?;
        assert_eq!(reason, "response=ErrorCode::HttpRequestDenied", "{authority}");
    }
    tokio::time::sleep(PROMPT).await;
    assert_eq!(
        outsider.connections(),
        0,
        "the guest reached a host outside the allowlist"
    );
    assert_eq!(allowed.connections(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_resolving_to_a_private_address_is_refused() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let server = ScriptedServer::start(Reply::ok(b"{}")).await?;
    let runtime = runtime(path, vec![server.allowed("localhost")?], server.trust(), DEADLINE)?;

    let reason = guest_failure(probe(&runtime, &server.authority("localhost"), "ana").await?)?;

    assert_eq!(reason, "response=ErrorCode::DestinationIpProhibited");
    tokio::time::sleep(PROMPT).await;
    assert_eq!(server.connections(), 0, "the guest reached a private address");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_to_a_host_outside_the_allowlist_is_refused() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let outsider = ScriptedServer::start(Reply::ok(b"{}")).await?;
    let location = format!("https://{}/ana", outsider.authority("127.0.0.1"));
    let mut redirecting = ScriptedServer::start(Reply::redirect(&location)).await?;
    let runtime = runtime(
        path,
        vec![redirecting.allowed("127.0.0.1")?],
        redirecting.trust(),
        DEADLINE,
    )?;

    let reason = guest_failure(probe(&runtime, &redirecting.authority("127.0.0.1"), "ana").await?)?;

    assert_eq!(reason, "response=ErrorCode::HttpRequestDenied");
    assert_eq!(redirecting.heads.try_recv()?.request_line(), "GET /ana HTTP/1.1");
    tokio::time::sleep(PROMPT).await;
    assert_eq!(outsider.connections(), 0, "the redirect was followed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_body_over_the_limit_is_refused() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let server = ScriptedServer::start(Reply::streamed(BODY_LIMIT + 1)).await?;
    let runtime = runtime(path, vec![server.allowed("127.0.0.1")?], server.trust(), DEADLINE)?;

    let reason = guest_failure(probe(&runtime, &server.authority("127.0.0.1"), "ana").await?)?;

    assert_eq!(reason, "body=\"HTTP response body size exceeds limit\"");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_body_over_the_limit_is_refused() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let declared = BODY_LIMIT + 1;
    let server = ScriptedServer::start(Reply::declared(declared)).await?;
    let runtime = runtime(path, vec![server.allowed("127.0.0.1")?], server.trust(), DEADLINE)?;

    let reason = guest_failure(probe(&runtime, &server.authority("127.0.0.1"), "ana").await?)?;

    assert_eq!(
        reason,
        format!("response=ErrorCode::HttpResponseBodySize(Some({declared}))")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_deadline_aborts_an_in_flight_guest_request() -> TestResult {
    let Some(path) = guest::http_probe_component() else {
        return Ok(());
    };
    let mut server = ScriptedServer::start(Reply::Hang).await?;
    let runtime = runtime(path, vec![server.allowed("127.0.0.1")?], server.trust(), SHORT_DEADLINE)?;

    let started = Instant::now();
    let outcome = probe(&runtime, &server.authority("127.0.0.1"), "ana").await?;
    let returned = Instant::now();

    assert_eq!(outcome, HookOutcome::Unavailable(HookFailure::Deadline));
    assert!(server.heads.try_recv().is_ok(), "the request never reached the server");
    let closed = tokio::time::timeout(PROMPT, server.closed.recv())
        .await
        .map_err(|_| "the connection stayed open after the deadline")?
        .ok_or("the server stopped")?;
    let elapsed = returned.saturating_duration_since(started);
    assert!(elapsed < SHORT_DEADLINE + PROMPT, "the call returned after {elapsed:?}");
    let lingering = closed.saturating_duration_since(returned);
    assert!(
        lingering < PROMPT,
        "connection closed {lingering:?} after the call returned"
    );
    Ok(())
}
