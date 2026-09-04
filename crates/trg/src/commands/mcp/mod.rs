mod auth;
mod proxy;

pub use proxy::ProxyArgs;

use clap::Subcommand;

use auth::AuthCommands;
use proxy::{run_mcp_daemon, ProxyError};

use crate::{
    config::ResolvedMcpServer,
    secrets::{Backend, SecretPath},
};

/// Everything `trg mcp` needs, resolved once in `main` and injected down.
///
/// Nothing below this point reads config or constructs a backend.
pub struct McpContext {
    pub server_name: String,
    pub profile: ResolvedMcpServer,
    pub backend: Backend,
    pub cred_path: SecretPath,
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// Bridge stdio JSON-RPC MCP to a configured remote MCP endpoint over HTTP.
    Proxy(ProxyArgs),

    /// Manage OAuth credentials stored for MCP servers.
    #[command(subcommand)]
    Auth(AuthCommands),
}

impl McpCommands {
    /// The configured server every `trg mcp` subcommand operates on. `main`
    /// needs this before it can resolve config, and it lives inside the parsed
    /// args rather than beside them.
    pub fn server_name(&self) -> &str {
        match self {
            McpCommands::Proxy(args) => args.server.trim(),
            McpCommands::Auth(cmd) => cmd.server_name(),
        }
    }

    pub async fn handle(self, ctx: &McpContext) -> i32 {
        match self {
            McpCommands::Proxy(_) => match run_mcp_daemon(ctx).await {
                Ok(()) => 0,
                Err(e) => emit_proxy_err(e),
            },
            McpCommands::Auth(cmd) => cmd.handle(ctx).await,
        }
    }
}

fn emit_proxy_err(e: ProxyError) -> i32 {
    eprintln!("{e}");
    1
}
