use std::collections::HashMap;
use std::fmt::Display;
use std::io::IsTerminal;
use std::time::Duration;

use http::HeaderValue;
use rmcp::{
    model::{ClientRequest, ErrorCode, ErrorData, JsonRpcMessage, JsonRpcRequest},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        async_rw::AsyncRwTransport,
        auth::AuthClient,
        stdio,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
        Transport,
    },
    RoleClient, RoleServer,
};
use secrecy::ExposeSecret;
use tracing::{debug, error, info, warn, Instrument, Span};

use super::telemetry::{Connection, Endpoint, ExitReason, ObservedHttpClient, RequestSpan, Session, SessionEnd};
use crate::{
    commands::mcp::McpContext,
    config::ResolvedMcpServer,
    oauth::{
        ensure_credentials_for,
        telemetry::{auth_error_type, record_error, OAuthHttp, OAuthStep},
        EnsureError, EnsureOutcome,
    },
    telemetry::ContentCapture,
};

#[derive(Debug, thiserror::Error)]
pub enum TransportBuildError {
    #[error("invalid header `{name}`: {cause}")]
    Header { name: String, cause: String },

    #[error("HTTP client: {0}")]
    Client(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("remote MCP transport configuration: {0}")]
    TransportCfg(#[from] TransportBuildError),

    #[error("remote MCP transport closed: {0}")]
    RemoteClosed(String),

    #[error("local stdio MCP transport closed: {0}")]
    LocalClosed(String),

    #[error("{0}")]
    Ensure(#[from] EnsureError),
}

impl ProxyError {
    fn error_type(&self) -> &'static str {
        match self {
            Self::TransportCfg(_) => "transport_config",
            Self::RemoteClosed(_) => "remote_closed",
            Self::LocalClosed(_) => "local_closed",
            Self::Ensure(error) => error.error_type(),
        }
    }
}

const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How a bridge stopped, before it is reported as a [`ProxyError`].
enum BridgeExit {
    HostEof,
    RemoteClosed(String),
    LocalClosed(String),
}

impl BridgeExit {
    fn reason(&self) -> ExitReason {
        match self {
            Self::HostEof => ExitReason::HostEof,
            Self::RemoteClosed(_) => ExitReason::RemoteClosed,
            Self::LocalClosed(_) => ExitReason::LocalClosed,
        }
    }

    fn into_result(self) -> Result<(), ProxyError> {
        match self {
            Self::HostEof => Ok(()),
            Self::RemoteClosed(cause) => Err(ProxyError::RemoteClosed(cause)),
            Self::LocalClosed(cause) => Err(ProxyError::LocalClosed(cause)),
        }
    }
}

pub async fn run_mcp_daemon(ctx: &McpContext) -> Result<(), ProxyError> {
    let resolved = ctx.endpoint()?;
    let mut session = Session::start(
        ctx.server_name.as_str(),
        Endpoint::parse(resolved.url.expose_secret()),
        ContentCapture::from_env(),
    );
    let span = session.span().clone();
    let result = serve(ctx, resolved, &mut session).instrument(span).await;
    match &result {
        Ok(exit) => session.finish(SessionEnd::Exited(exit.reason())),
        Err(error) => session.finish(SessionEnd::Failed(error.error_type())),
    }
    result.and_then(BridgeExit::into_result)
}

async fn serve(
    ctx: &McpContext,
    resolved: &ResolvedMcpServer,
    session: &mut Session,
) -> Result<BridgeExit, ProxyError> {
    let server_name = ctx.server_name.as_str();
    info!(
        server = server_name,
        pid = std::process::id(),
        headers = resolved.http_headers.len(),
        "startup"
    );

    let http_conf = streamable_http_config(resolved)?;

    let ensured = match OAuthHttp::traced() {
        Ok(http) => {
            ensure_credentials_for(
                resolved,
                server_name,
                &ctx.backend,
                &ctx.cred_path,
                ctx.fallback.as_ref(),
                &http,
            )
            .await
        }
        Err(e) => Err(EnsureError::from(e)),
    };
    let outcome = match ensured {
        Ok(outcome) => outcome,
        Err(e) => {
            error!(server = server_name, error = %e, "ensure_credentials failed");
            refuse_over_stdio(&e).await;
            return Err(e.into());
        }
    };
    session.record_auth(outcome.auth_outcome());

    let exit = match outcome {
        EnsureOutcome::NoAuthRequired => {
            info!(server = server_name, "auth: none required, using plain client");
            let client = ObservedHttpClient::new(default_http_client()?, session);
            let remote = StreamableHttpClientTransport::with_client(client, http_conf);
            bridge_stdio_to_remote(remote, session).await
        }
        EnsureOutcome::AlreadyAuthorized(manager) | EnsureOutcome::Authorized(manager) => {
            info!(server = server_name, "auth: using AuthClient with stored credentials");
            let auth_client = AuthClient::new(reqwest::Client::new(), manager);
            refresh_access_token(&auth_client, server_name).await;
            let client = ObservedHttpClient::new(auth_client, session);
            let remote = StreamableHttpClientTransport::with_client(client, http_conf);
            bridge_stdio_to_remote(remote, session).await
        }
    };

    match &exit {
        BridgeExit::HostEof => info!(server = server_name, "bridge exited cleanly"),
        BridgeExit::RemoteClosed(cause) | BridgeExit::LocalClosed(cause) => {
            warn!(server = server_name, error = %cause, "bridge exited with error")
        }
    }
    Ok(exit)
}

/// Fetches the access token once up front, refreshing it if it has expired,
/// so the refresh shows in the trace as its own step rather than inside
/// whichever request happened to need it first. A failure is left for the
/// first request to meet, exactly as it would be without this.
async fn refresh_access_token(client: &AuthClient<reqwest::Client>, server_name: &str) {
    let span = OAuthStep::RefreshAccessToken.span(server_name);
    if let Err(e) = client.get_access_token().instrument(span.clone()).await {
        record_error(&span, auth_error_type(&e));
        warn!(server = server_name, error = %e, "access token refresh failed");
    }
}

/// The client rmcp's `from_config` would build, which it does not expose.
fn default_http_client() -> Result<reqwest::Client, TransportBuildError> {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| TransportBuildError::Client(e.to_string()))
}

/// Answer the host over the protocol instead of dying before one exists.
///
/// A proxy can fail before its bridge is built: while reading config, while
/// picking a backend, or while ensuring credentials. All three used to leave
/// stdout empty and the reason on a stderr an editor discards, so the host
/// could report only that its MCP server had exited. Every request gets the
/// reason back instead, starting with `initialize`, which is the one an editor
/// puts in front of the person who has to act on it.
///
/// Skipped when stdin is a terminal, where there is no host to answer and
/// waiting for a request that will never be typed would hang a `trg mcp proxy`
/// run by hand.
pub async fn refuse_over_stdio(reason: &dyn Display) {
    if std::io::stdin().is_terminal() {
        return;
    }

    let reason = reason.to_string();

    let (stdin, stdout) = stdio();
    let mut local = AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout);

    while let Some(msg) = local.receive().await {
        // A notification expects no answer, and replying to one with an id it
        // never carried is worse than staying quiet.
        let JsonRpcMessage::Request(request) = msg else {
            continue;
        };

        if let Err(e) = local.send(refusal(&request, &reason)).await {
            warn!(error = %e, "refusal: local send failed");
            break;
        }
    }

    let _ = local.close().await;
}

/// The error answering `request`, recorded as a failed request span under
/// whatever span is current.
fn refusal(request: &JsonRpcRequest<ClientRequest>, reason: &str) -> TxJsonRpcMessage<RoleServer> {
    let error = ErrorData::new(ErrorCode::INTERNAL_ERROR, reason.to_string(), None);
    RequestSpan::open(
        &request.request,
        &request.id,
        &Span::current(),
        &Connection::default(),
        ContentCapture::from_env(),
    )
    .refuse(&error);
    JsonRpcMessage::error(error, Some(request.id.clone()))
}

async fn bridge_stdio_to_remote<C>(mut remote: StreamableHttpClientTransport<C>, session: &mut Session) -> BridgeExit
where
    C: StreamableHttpClient + Send + Sync + 'static,
{
    let (stdin, stdout) = stdio();
    let mut local = AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout);
    debug!("bridge: entering loop");

    let exit = loop {
        tokio::select! {
            host_msg = local.receive() => {
                let Some(msg) = host_msg else {
                    debug!("bridge: host stdin closed (EOF)");
                    let _ = remote.close().await;
                    let _ = local.close().await;
                    return BridgeExit::HostEof;
                };

                debug!("bridge: host -> remote");
                let mut msg = msg;
                session.on_host_message(&mut msg);
                let forward: TxJsonRpcMessage<RoleClient> = host_receive_to_remote_send(msg);

                if let Err(e) = remote.send(forward).await {
                    warn!(error = %e, "bridge: remote send failed");
                    break BridgeExit::RemoteClosed(e.to_string());
                }
            }

            srv_msg = remote.receive() => {
                let Some(msg) = srv_msg else {
                    warn!("bridge: remote disconnected");
                    break BridgeExit::RemoteClosed("remote disconnected".into());
                };
                let msg: RxJsonRpcMessage<RoleClient> = msg;
                session.on_remote_message(&msg);

                debug!("bridge: remote -> host");
                let back: TxJsonRpcMessage<RoleServer> = remote_receive_to_host_send(msg);

                if let Err(e) = local.send(back).await {
                    warn!(error = %e, "bridge: local send failed");
                    break BridgeExit::LocalClosed(e.to_string());
                }
            }
        }
    };

    // The transport worker holds the session span through its client, and a
    // worker merely dropped can outlive the telemetry flush at exit.
    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, remote.close()).await;
    exit
}

fn host_receive_to_remote_send(msg: RxJsonRpcMessage<RoleServer>) -> TxJsonRpcMessage<RoleClient> {
    msg
}

fn remote_receive_to_host_send(msg: RxJsonRpcMessage<RoleClient>) -> TxJsonRpcMessage<RoleServer> {
    msg
}

fn streamable_http_config(cfg: &ResolvedMcpServer) -> Result<StreamableHttpClientTransportConfig, TransportBuildError> {
    let mut custom_headers = HashMap::new();

    for (name, secret) in &cfg.http_headers {
        let value = secret.expose_secret();
        custom_headers.insert(
            name.clone(),
            HeaderValue::try_from(value).map_err(|e| TransportBuildError::Header {
                name: name.to_string(),
                cause: e.to_string(),
            })?,
        );
    }

    let mut cfg_out = StreamableHttpClientTransportConfig::with_uri(cfg.url.expose_secret());
    cfg_out.custom_headers = custom_headers;
    cfg_out.allow_stateless = true;
    Ok(cfg_out)
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::Status;
    use opentelemetry_semantic_conventions::attribute::{ERROR_TYPE, RPC_RESPONSE_STATUS_CODE};
    use serde_json::json;

    use super::*;
    use crate::telemetry::testing::capture;

    #[test]
    fn a_refused_request_is_an_error_span() {
        let request: JsonRpcRequest<ClientRequest> = serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": 4, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "host", "version": "1" },
            },
        }))
        .expect("valid request");

        let (answer, trace) = capture(|| {
            let root = tracing::info_span!("trg");
            root.in_scope(|| refusal(&request, "OAuth: not a terminal"))
        });

        trace.assert_child_of("initialize", "trg");
        let code = ErrorCode::INTERNAL_ERROR.0.to_string();
        assert_eq!(
            trace
                .attribute("initialize", RPC_RESPONSE_STATUS_CODE)
                .map(|v| v.to_string()),
            Some(code.clone())
        );
        assert_eq!(
            trace.attribute("initialize", ERROR_TYPE).map(|v| v.to_string()),
            Some(code)
        );
        assert!(matches!(
            trace.span("initialize").map(|s| &s.status),
            Some(Status::Error { .. })
        ));
        assert_eq!(
            serde_json::to_value(&answer).expect("serializable")["error"]["message"],
            json!("OAuth: not a terminal")
        );
    }
}
