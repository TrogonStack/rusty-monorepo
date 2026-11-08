mod auth;
mod proxy;

pub use proxy::ProxyArgs;

use std::fmt::Display;

use clap::Subcommand;

use crate::exit_code::ExitCode;
use auth::AuthCommands;
use proxy::{refuse_over_stdio, run_mcp_daemon, ProxyError};

use crate::config::ResolvedMcpServer;
use crate::oauth::EnsureError;
use crate::secrets::{Backend, SecretPath};

/// Everything `trg mcp` needs, resolved once in `main` and injected down.
///
/// Nothing below this point reads config or constructs a backend.
pub struct McpContext {
    pub server_name: String,
    pub backend: Backend,
    pub cred_path: SecretPath,
    /// The pre-`machine_id` shared path, for the OAuth credential store to
    /// fall back onto when `cred_path` is empty. `None` when the backend has
    /// no such second layout to fall back to.
    pub fallback: Option<SecretPath>,
    /// Absent for the commands that only touch stored credentials. Resolving
    /// one can mean reading a secrets backend, and `auth logout` is the
    /// documented way out of a broken credential, so it must not depend on a
    /// backend being reachable to run.
    profile: Option<ResolvedMcpServer>,
}

impl McpContext {
    pub fn with_endpoint(
        server_name: String,
        backend: Backend,
        cred_path: SecretPath,
        fallback: Option<SecretPath>,
        profile: ResolvedMcpServer,
    ) -> Self {
        Self {
            server_name,
            backend,
            cred_path,
            fallback,
            profile: Some(profile),
        }
    }

    pub fn credentials_only(
        server_name: String,
        backend: Backend,
        cred_path: SecretPath,
        fallback: Option<SecretPath>,
    ) -> Self {
        Self {
            server_name,
            backend,
            cred_path,
            fallback,
            profile: None,
        }
    }

    pub fn endpoint(&self) -> Result<&ResolvedMcpServer, EnsureError> {
        self.profile
            .as_ref()
            .ok_or_else(|| EnsureError::NoEndpoint(self.server_name.clone()))
    }
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

    /// Whether this command reaches the server's endpoint, and so needs a
    /// resolved URL and headers. The two that only read or delete stored
    /// credentials do not, and skipping the resolution keeps them working when
    /// a var's backend is unreachable.
    pub fn needs_endpoint(&self) -> bool {
        match self {
            McpCommands::Proxy(_) => true,
            McpCommands::Auth(cmd) => cmd.needs_endpoint(),
        }
    }

    pub async fn handle(self, ctx: &McpContext) -> ExitCode {
        match self {
            McpCommands::Proxy(_) => match run_mcp_daemon(ctx).await {
                Ok(()) => ExitCode::Success,
                Err(e) => emit_proxy_err(e),
            },
            McpCommands::Auth(cmd) => cmd.handle(ctx).await,
        }
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn emit_proxy_err(e: ProxyError) -> ExitCode {
    eprintln!("{e}");
    ExitCode::Failed
}

/// Report a failure that happened before [`McpCommands::handle`] could run.
///
/// Config and the backend are resolved in `main`, so a proxy can be dead before
/// it owns anything. To the editor that spawned it those failures look exactly
/// like the ones the bridge reports, and they need the same channel.
///
/// Only the proxy gets that treatment. Every other subcommand is typed by a
/// person who is already looking at stderr.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub async fn report_startup_failure(command: &McpCommands, error: &dyn Display) -> ExitCode {
    eprintln!("{error}");
    if matches!(command, McpCommands::Proxy(_)) {
        refuse_over_stdio(command.server_name(), error).await;
    }
    ExitCode::Failed
}

#[cfg(test)]
mod tests;
