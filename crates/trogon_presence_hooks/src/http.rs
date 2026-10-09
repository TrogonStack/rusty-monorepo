use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{HeaderValue, CONTENT_LENGTH, HOST};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{timeout_at, Instant};
use tokio_rustls::TlsConnector;
use wasmtime_wasi_http::io::TokioIo;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{hyper_request_error, hyper_response_error, HttpResult, WasiHttpHooks};

use crate::config::{AllowedHost, HttpsPort, TrustAnchors};

pub(crate) const BODY_MAX_BYTES: usize = 64 * 1024;
const OUTGOING_BODY_CHUNKS: usize = 4;
const OUTGOING_BODY_CHUNK_BYTES: usize = BODY_MAX_BYTES / OUTGOING_BODY_CHUNKS;
const OUTSTANDING_REQUESTS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressClass {
    Public,
    Local,
}

impl AddressClass {
    fn of(ip: IpAddr) -> Self {
        let local = match ip {
            IpAddr::V4(v4) => {
                let [a, b, ..] = v4.octets();
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    || v4.is_multicast()
                    || (a == 100 && (64..128).contains(&b))
            }
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => return Self::of(IpAddr::V4(v4)),
                None => {
                    let first = v6.segments()[0];
                    v6.is_loopback()
                        || v6.is_unspecified()
                        || v6.is_multicast()
                        || (first & 0xfe00) == 0xfc00
                        || (first & 0xffc0) == 0xfe80
                }
            },
        };
        if local {
            Self::Local
        } else {
            Self::Public
        }
    }
}

pub(crate) struct OutboundPolicy {
    targets: Arc<[AllowedHost]>,
    tls: Arc<ClientConfig>,
}

#[derive(Debug, thiserror::Error)]
#[error("could not configure the hook TLS client: {0}")]
pub struct TlsSetupError(String);

impl OutboundPolicy {
    pub(crate) fn new(targets: &[AllowedHost], trust: &TrustAnchors) -> Result<Self, TlsSetupError> {
        let mut roots = RootCertStore::empty();
        match trust {
            TrustAnchors::WebPkiRoots => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            TrustAnchors::Pinned(certificates) => {
                for certificate in certificates {
                    roots
                        .add(certificate.clone())
                        .map_err(|err| TlsSetupError(err.to_string()))?;
                }
            }
        }
        let tls = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|err| TlsSetupError(err.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            targets: targets.into(),
            tls: Arc::new(tls),
        })
    }

    fn target(
        &self,
        request: &hyper::Request<HyperOutgoingBody>,
        config: &OutgoingRequestConfig,
    ) -> Option<AllowedHost> {
        let uri = request.uri();
        if !config.use_tls || uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return None;
        }
        let host = uri.host()?;
        let port = match uri.port_u16() {
            None => HttpsPort::DEFAULT,
            Some(port) => HttpsPort::try_from(port).ok()?,
        };
        self.targets
            .iter()
            .find(|allowed| allowed.matches_authority(host, port))
            .cloned()
    }
}

pub(crate) struct HttpsTransport {
    policy: Arc<OutboundPolicy>,
    outstanding: Arc<Semaphore>,
    expires: Instant,
}

impl HttpsTransport {
    pub(crate) fn new(policy: Arc<OutboundPolicy>, expires: Instant) -> Self {
        Self {
            policy,
            outstanding: Arc::new(Semaphore::new(OUTSTANDING_REQUESTS)),
            expires,
        }
    }
}

impl WasiHttpHooks for HttpsTransport {
    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let Some(target) = self.policy.target(&request, &config) else {
            tracing::warn!(uri = %request.uri(), "hook outbound request denied by the HTTPS allowlist");
            return Err(ErrorCode::HttpRequestDenied.into());
        };
        let Ok(permit) = self.outstanding.clone().try_acquire_owned() else {
            tracing::warn!(target = %target, "hook outbound request denied, too many outstanding requests");
            return Err(ErrorCode::HttpRequestDenied.into());
        };
        let tls = self.policy.tls.clone();
        let expires = self.expires;
        let handle =
            wasmtime_wasi::runtime::spawn(async move { Ok(send(target, tls, request, config, expires, permit).await) });
        Ok(HostFutureIncomingResponse::pending(handle))
    }

    fn outgoing_body_buffer_chunks(&mut self) -> usize {
        OUTGOING_BODY_CHUNKS
    }

    fn outgoing_body_chunk_size(&mut self) -> usize {
        OUTGOING_BODY_CHUNK_BYTES
    }
}

async fn resolve(target: &AllowedHost) -> Result<SocketAddr, ErrorCode> {
    let port = target.port().get();
    if let Some(ip) = target.ip() {
        return Ok(SocketAddr::new(ip, port));
    }
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((target.host(), port))
        .await
        .map_err(|_| dns_failure())?
        .collect();
    if addresses.is_empty() {
        return Err(dns_failure());
    }
    addresses
        .into_iter()
        .find(|address| AddressClass::of(address.ip()) == AddressClass::Public)
        .ok_or(ErrorCode::DestinationIpProhibited)
}

fn dns_failure() -> ErrorCode {
    ErrorCode::DnsError(wasmtime_wasi_http::p2::bindings::http::types::DnsErrorPayload {
        rcode: Some("address not available".to_owned()),
        info_code: Some(0),
    })
}

async fn send(
    target: AllowedHost,
    tls: Arc<ClientConfig>,
    request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
    expires: Instant,
    permit: OwnedSemaphorePermit,
) -> Result<IncomingResponse, ErrorCode> {
    match timeout_at(expires, exchange(target, tls, request, config, expires, permit)).await {
        Ok(result) => result,
        Err(_) => Err(ErrorCode::ConnectionTimeout),
    }
}

async fn exchange(
    target: AllowedHost,
    tls: Arc<ClientConfig>,
    request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
    expires: Instant,
    permit: OwnedSemaphorePermit,
) -> Result<IncomingResponse, ErrorCode> {
    let (mut parts, body) = request.into_parts();
    let body = Limited::new(body, BODY_MAX_BYTES)
        .collect()
        .await
        .map_err(|_| ErrorCode::HttpRequestBodySize(None))?
        .to_bytes();
    let address = resolve(&target).await?;
    let connect_by = expires.min(Instant::now() + config.connect_timeout);
    let tcp = timeout_at(connect_by, TcpStream::connect(address))
        .await
        .map_err(|_| ErrorCode::ConnectionTimeout)?
        .map_err(|_| ErrorCode::ConnectionRefused)?;
    let server_name = match target.ip() {
        Some(ip) => ServerName::IpAddress(ip.into()),
        None => ServerName::try_from(target.host().to_owned()).map_err(|_| ErrorCode::HttpRequestUriInvalid)?,
    };
    let stream = timeout_at(connect_by, TlsConnector::from(tls).connect(server_name, tcp))
        .await
        .map_err(|_| ErrorCode::ConnectionTimeout)?
        .map_err(|err| {
            tracing::warn!(%err, target = %target, "hook outbound TLS handshake failed");
            ErrorCode::TlsProtocolError
        })?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(hyper_request_error)?;
    let worker = wasmtime_wasi::runtime::spawn(async move {
        let _permit = permit;
        if let Err(err) = connection.await {
            tracing::debug!(%err, "hook outbound connection closed with an error");
        }
    });
    let authority = HeaderValue::from_str(&target.to_string()).map_err(|_| ErrorCode::HttpRequestUriInvalid)?;
    parts.headers.insert(HOST, authority);
    parts.uri = parts
        .uri
        .path_and_query()
        .map_or_else(|| http::Uri::from_static("/"), |path| http::Uri::from(path.clone()));
    let outgoing = hyper::Request::from_parts(parts, Full::new(body));
    let first_byte_by = expires.min(Instant::now() + config.first_byte_timeout);
    let response = timeout_at(first_byte_by, sender.send_request(outgoing))
        .await
        .map_err(|_| ErrorCode::ConnectionReadTimeout)?
        .map_err(hyper_request_error)?;
    if response.status().is_redirection() {
        tracing::warn!(status = %response.status(), target = %target, "hook outbound redirect refused");
        return Err(ErrorCode::HttpRequestDenied);
    }
    let declared = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(length) = declared.filter(|length| *length > BODY_MAX_BYTES as u64) {
        return Err(ErrorCode::HttpResponseBodySize(Some(length)));
    }
    let between_bytes_timeout = config.between_bytes_timeout.min(
        expires
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)),
    );
    let resp = response.map(|body| {
        Limited::new(body.map_err(hyper_response_error), BODY_MAX_BYTES)
            .map_err(|err| match err.downcast::<ErrorCode>() {
                Ok(code) => *code,
                Err(_) => ErrorCode::HttpResponseBodySize(None),
            })
            .boxed_unsync()
    });
    Ok(IncomingResponse {
        resp,
        worker: Some(worker),
        between_bytes_timeout,
    })
}

#[cfg(test)]
mod tests {
    use http_body_util::Empty;
    use hyper::body::Bytes;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::*;

    struct LocalServer {
        port: u16,
        certificate: CertificateDer<'static>,
    }

    impl LocalServer {
        async fn start(response: Vec<u8>) -> Self {
            let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("certificate");
            let certificate = generated.cert.der().clone();
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()));
            let tls = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("protocol versions")
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key)
                .expect("server config");
            let acceptor = TlsAcceptor::from(Arc::new(tls));
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let port = listener.local_addr().expect("local addr").port();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    let response = response.clone();
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
                        let _ = stream.write_all(&response).await;
                        let _ = stream.flush().await;
                    });
                }
            });
            Self { port, certificate }
        }

        fn transport(&self) -> HttpsTransport {
            let allowed: AllowedHost = format!("127.0.0.1:{}", self.port).parse().expect("allowed host");
            let trust = TrustAnchors::Pinned(vec![self.certificate.clone()]);
            let policy = OutboundPolicy::new(&[allowed], &trust).expect("outbound policy");
            HttpsTransport::new(Arc::new(policy), Instant::now() + Duration::from_secs(5))
        }

        fn uri(&self) -> String {
            format!("https://127.0.0.1:{}/status", self.port)
        }
    }

    fn request(uri: &str, body: HyperOutgoingBody) -> hyper::Request<HyperOutgoingBody> {
        hyper::Request::builder().uri(uri).body(body).expect("request")
    }

    fn empty() -> HyperOutgoingBody {
        Empty::<Bytes>::new().map_err(|never| match never {}).boxed_unsync()
    }

    fn tls() -> OutgoingRequestConfig {
        OutgoingRequestConfig {
            use_tls: true,
            connect_timeout: Duration::from_secs(5),
            first_byte_timeout: Duration::from_secs(5),
            between_bytes_timeout: Duration::from_secs(5),
        }
    }

    async fn send_through(
        transport: &mut HttpsTransport,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> Result<IncomingResponse, ErrorCode> {
        match transport.send_request(request, config) {
            Ok(HostFutureIncomingResponse::Pending(handle)) => handle.await.expect("transport task"),
            Ok(_) => panic!("the transport must hand back a pending response"),
            Err(err) => Err(err.downcast().expect("an error code")),
        }
    }

    fn denied(result: Result<IncomingResponse, ErrorCode>) -> bool {
        matches!(result, Err(ErrorCode::HttpRequestDenied))
    }

    #[tokio::test]
    async fn returns_a_bounded_response_from_an_allowed_host() {
        let server = LocalServer::start(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok".to_vec()).await;
        let response = send_through(&mut server.transport(), request(&server.uri(), empty()), tls())
            .await
            .expect("response");
        assert_eq!(response.resp.status(), 200);
        let body = response.resp.into_body().collect().await.expect("body").to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn refuses_redirects() {
        let server = LocalServer::start(
            b"HTTP/1.1 302 Found\r\nlocation: https://example.com/\r\ncontent-length: 0\r\n\r\n".to_vec(),
        )
        .await;
        let result = send_through(&mut server.transport(), request(&server.uri(), empty()), tls()).await;
        assert!(denied(result));
    }

    #[tokio::test]
    async fn refuses_a_declared_oversized_body() {
        let declared = BODY_MAX_BYTES as u64 + 1;
        let server =
            LocalServer::start(format!("HTTP/1.1 200 OK\r\ncontent-length: {declared}\r\n\r\n").into_bytes()).await;
        let result = send_through(&mut server.transport(), request(&server.uri(), empty()), tls()).await;
        assert!(matches!(result, Err(ErrorCode::HttpResponseBodySize(Some(length))) if length == declared));
    }

    #[tokio::test]
    async fn refuses_a_streamed_oversized_body() {
        let chunk = vec![b'x'; BODY_MAX_BYTES + 1];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n",
            chunk.len()
        )
        .into_bytes();
        response.extend_from_slice(&chunk);
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let server = LocalServer::start(response).await;
        let response = send_through(&mut server.transport(), request(&server.uri(), empty()), tls())
            .await
            .expect("headers arrive");
        let read = response.resp.into_body().collect().await;
        assert!(matches!(read, Err(ErrorCode::HttpResponseBodySize(None))));
    }

    #[tokio::test]
    async fn refuses_an_oversized_request_body() {
        let server = LocalServer::start(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec()).await;
        let body = Full::new(Bytes::from(vec![b'x'; BODY_MAX_BYTES + 1]))
            .map_err(|never| match never {})
            .boxed_unsync();
        let result = send_through(&mut server.transport(), request(&server.uri(), body), tls()).await;
        assert!(matches!(result, Err(ErrorCode::HttpRequestBodySize(None))));
    }

    #[tokio::test]
    async fn denies_targets_outside_the_allowlist() {
        let server = LocalServer::start(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec()).await;
        let mut transport = server.transport();
        let plain = OutgoingRequestConfig {
            use_tls: false,
            ..tls()
        };
        let http_uri = format!("http://127.0.0.1:{}/status", server.port);
        assert!(denied(
            send_through(&mut transport, request(&http_uri, empty()), plain).await
        ));
        let other_port = format!("https://127.0.0.1:{}/status", server.port.wrapping_add(1).max(1));
        assert!(denied(
            send_through(&mut transport, request(&other_port, empty()), tls()).await
        ));
        let other_host = format!("https://localhost:{}/status", server.port);
        assert!(denied(
            send_through(&mut transport, request(&other_host, empty()), tls()).await
        ));
    }

    #[tokio::test]
    async fn rejects_untrusted_certificates() {
        let server = LocalServer::start(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec()).await;
        let allowed: AllowedHost = format!("127.0.0.1:{}", server.port).parse().expect("allowed host");
        let policy = OutboundPolicy::new(&[allowed], &TrustAnchors::WebPkiRoots).expect("outbound policy");
        let mut transport = HttpsTransport::new(Arc::new(policy), Instant::now() + Duration::from_secs(5));
        let result = send_through(&mut transport, request(&server.uri(), empty()), tls()).await;
        assert!(matches!(result, Err(ErrorCode::TlsProtocolError)));
    }

    #[tokio::test]
    async fn denies_a_third_outstanding_request() {
        let stalled = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = stalled.local_addr().expect("local addr").port();
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((tcp, _)) = stalled.accept().await {
                open.push(tcp);
            }
        });
        let allowed: AllowedHost = format!("127.0.0.1:{port}").parse().expect("allowed host");
        let policy = OutboundPolicy::new(&[allowed], &TrustAnchors::WebPkiRoots).expect("outbound policy");
        let mut transport = HttpsTransport::new(Arc::new(policy), Instant::now() + Duration::from_secs(5));
        let uri = format!("https://127.0.0.1:{port}/status");
        let first = transport
            .send_request(request(&uri, empty()), tls())
            .expect("first admitted");
        let second = transport
            .send_request(request(&uri, empty()), tls())
            .expect("second admitted");
        let third = transport.send_request(request(&uri, empty()), tls());
        assert!(matches!(
            third.map_err(|err| err.downcast().expect("an error code")),
            Err(ErrorCode::HttpRequestDenied)
        ));
        drop((first, second));
        held.abort();
    }
}
