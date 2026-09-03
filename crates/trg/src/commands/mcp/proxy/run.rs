use std::collections::HashMap;

use http::HeaderValue;
use rmcp::{
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
use tracing::{debug, error, info, warn};

use crate::{
    commands::mcp::McpContext,
    config::ResolvedMcpServer,
    oauth::{ensure_credentials_for, EnsureError, EnsureOutcome},
};

#[derive(Debug, thiserror::Error)]
pub enum TransportBuildError {
    #[error("invalid header `{name}`: {cause}")]
    Header { name: String, cause: String },
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

pub async fn run_mcp_daemon(ctx: &McpContext) -> Result<(), ProxyError> {
    let server_name = ctx.server_name.as_str();
    let resolved = &ctx.profile;
    info!(
        server = server_name,
        pid = std::process::id(),
        headers = resolved.http_headers.len(),
        "startup"
    );

    let http_conf = streamable_http_config(resolved)?;

    let outcome = match ensure_credentials_for(resolved, server_name, &ctx.backend, &ctx.cred_path).await {
        Ok(o) => o,
        Err(e) => {
            error!(server = server_name, error = %e, "ensure_credentials failed");
            return Err(e.into());
        }
    };

    let result = match outcome {
        EnsureOutcome::NoAuthRequired => {
            info!(server = server_name, "auth: none required, using plain client");
            let remote = StreamableHttpClientTransport::<reqwest::Client>::from_config(http_conf);
            bridge_stdio_to_remote(remote).await
        }
        EnsureOutcome::AlreadyAuthorized(manager) | EnsureOutcome::Authorized(manager) => {
            info!(server = server_name, "auth: using AuthClient with stored credentials");
            let auth_client = AuthClient::new(reqwest::Client::new(), manager);
            let remote = StreamableHttpClientTransport::with_client(auth_client, http_conf);
            bridge_stdio_to_remote(remote).await
        }
    };

    match &result {
        Ok(()) => info!(server = server_name, "bridge exited cleanly"),
        Err(e) => warn!(server = server_name, error = %e, "bridge exited with error"),
    }
    result
}

async fn bridge_stdio_to_remote<C>(mut remote: StreamableHttpClientTransport<C>) -> Result<(), ProxyError>
where
    C: StreamableHttpClient + Send + Sync + 'static,
{
    let (stdin, stdout) = stdio();
    let mut local = AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout);
    debug!("bridge: entering loop");

    loop {
        tokio::select! {
            host_msg = local.receive() => {
                let Some(msg) = host_msg else {
                    debug!("bridge: host stdin closed (EOF)");
                    let _ = remote.close().await;
                    let _ = local.close().await;
                    return Ok(());
                };

                debug!("bridge: host -> remote");
                let forward: TxJsonRpcMessage<RoleClient> = host_receive_to_remote_send(msg);

                if let Err(e) = remote.send(forward).await {
                    warn!(error = %e, "bridge: remote send failed");
                    return Err(ProxyError::RemoteClosed(e.to_string()));
                }
            }

            srv_msg = remote.receive() => {
                let Some(msg) = srv_msg else {
                    warn!("bridge: remote disconnected");
                    return Err(ProxyError::RemoteClosed("remote disconnected".into()));
                };
                let msg: RxJsonRpcMessage<RoleClient> = msg;

                debug!("bridge: remote -> host");
                let back: TxJsonRpcMessage<RoleServer> = remote_receive_to_host_send(msg);

                if let Err(e) = local.send(back).await {
                    warn!(error = %e, "bridge: local send failed");
                    return Err(ProxyError::LocalClosed(e.to_string()));
                }
            }
        }
    }
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
