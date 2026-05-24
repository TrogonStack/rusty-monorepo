mod auth;
mod proxy;

pub use proxy::ProxyArgs;

use clap::Subcommand;

use auth::AuthCommands;
use proxy::{run_mcp_daemon, ProxyError};

#[derive(Subcommand)]
pub enum McpCommands {
    /// Bridge stdio JSON-RPC MCP to a configured remote MCP endpoint over HTTP.
    Proxy(ProxyArgs),

    /// Manage OAuth credentials stored for MCP servers (macOS Keychain).
    #[command(subcommand)]
    Auth(AuthCommands),
}

impl McpCommands {
    pub async fn handle(self) -> i32 {
        match self {
            McpCommands::Proxy(args) => match run_mcp_daemon(&args).await {
                Ok(()) => 0,
                Err(e) => emit_proxy_err(e),
            },
            McpCommands::Auth(cmd) => cmd.handle().await,
        }
    }
}

fn emit_proxy_err(e: ProxyError) -> i32 {
    eprintln!("{e}");
    1
}
