use std::collections::HashMap;

use http::HeaderValue;
use rmcp::{
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        async_rw::AsyncRwTransport,
        stdio,
        streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig},
        Transport,
    },
    RoleClient, RoleServer,
};
use secrecy::ExposeSecret;

use crate::config::{self, ResolvedMcpServer};

use super::cli::ProxyArgs;

#[derive(Debug, thiserror::Error)]
pub enum TransportBuildError {
    #[error("invalid header `{name}`: {cause}")]
    Header { name: String, cause: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),

    #[error("remote MCP transport configuration: {0}")]
    TransportCfg(#[from] TransportBuildError),

    #[error("remote MCP transport closed: {0}")]
    RemoteClosed(String),

    #[error("local stdio MCP transport closed: {0}")]
    LocalClosed(String),
}

pub async fn run_mcp_daemon(args: &ProxyArgs) -> Result<(), ProxyError> {
    let resolved = config::load_mcp_server(args.server.trim())?;

    bridge_stdio_to_remote(resolved).await
}

type RemoteTransport = StreamableHttpClientTransport<reqwest::Client>;

async fn bridge_stdio_to_remote(profile: ResolvedMcpServer) -> Result<(), ProxyError> {
    let http_conf = streamable_http_config(&profile)?;

    let mut remote: RemoteTransport = RemoteTransport::from_config(http_conf);
    let (stdin, stdout) = stdio();

    let mut local = AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout);

    loop {
        tokio::select! {
            host_msg = local.receive() => {
                let Some(msg) = host_msg else {
                    let _ = remote.close().await;
                    let _ = local.close().await;
                    return Ok(());
                };

                let forward: TxJsonRpcMessage<RoleClient> = host_receive_to_remote_send(msg);

                remote
                    .send(forward)
                    .await
                    .map_err(|e| ProxyError::RemoteClosed(e.to_string()))?;
            }

            srv_msg = remote.receive() => {
                let msg: RxJsonRpcMessage<RoleClient> =
                    srv_msg.ok_or_else(|| ProxyError::RemoteClosed("remote disconnected".into()))?;

                let back: TxJsonRpcMessage<RoleServer> = remote_receive_to_host_send(msg);

                local
                    .send(back)
                    .await
                    .map_err(|e| ProxyError::LocalClosed(e.to_string()))?;
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
