mod auth;
mod proxy;

pub use proxy::ProxyArgs;

use std::fmt::Display;

use clap::Subcommand;

use auth::AuthCommands;
use proxy::{refuse_over_stdio, run_mcp_daemon, ProxyError};

use crate::{
    config::ResolvedMcpServer,
    oauth::EnsureError,
    secrets::{Backend, SecretPath},
};

/// Everything `trg mcp` needs, resolved once in `main` and injected down.
///
/// Nothing below this point reads config or constructs a backend.
pub struct McpContext {
    pub server_name: String,
    pub backend: Backend,
    pub cred_path: SecretPath,
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
        profile: ResolvedMcpServer,
    ) -> Self {
        Self {
            server_name,
            backend,
            cred_path,
            profile: Some(profile),
        }
    }

    pub fn credentials_only(server_name: String, backend: Backend, cred_path: SecretPath) -> Self {
        Self {
            server_name,
            backend,
            cred_path,
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

/// Report a failure that happened before [`McpCommands::handle`] could run.
///
/// Config and the backend are resolved in `main`, so a proxy can be dead before
/// it owns anything. To the editor that spawned it those failures look exactly
/// like the ones the bridge reports, and they need the same channel.
///
/// Only the proxy gets that treatment. Every other subcommand is typed by a
/// person who is already looking at stderr.
pub async fn report_startup_failure(command: &McpCommands, error: &dyn Display) -> i32 {
    eprintln!("{error}");
    if matches!(command, McpCommands::Proxy(_)) {
        refuse_over_stdio(error).await;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: McpCommands,
    }

    fn parse(args: &[&str]) -> McpCommands {
        TestCli::try_parse_from(std::iter::once("trg").chain(args.iter().copied()))
            .expect("parse")
            .command
    }

    #[test]
    fn proxy_and_login_reach_the_endpoint() {
        assert!(parse(&["proxy", "--server", "x"]).needs_endpoint());
        assert!(parse(&["auth", "login", "--server", "x"]).needs_endpoint());
    }

    /// The two commands that recover from a credential that will not load must
    /// not need a var's backend to be reachable first.
    #[test]
    fn status_and_logout_do_not_reach_the_endpoint() {
        assert!(!parse(&["auth", "status", "--server", "x"]).needs_endpoint());
        assert!(!parse(&["auth", "logout", "--server", "x"]).needs_endpoint());
    }

    fn backend() -> Backend {
        Backend::Fake(crate::secrets::fake::FakeBackend::new())
    }

    fn path() -> SecretPath {
        SecretPath::parse("mcp/x").expect("path")
    }

    fn profile() -> ResolvedMcpServer {
        ResolvedMcpServer {
            secrets: None,
            url: secrecy::SecretString::from("https://example.com/mcp".to_string()),
            transport: None,
            max_disconnected_time: None,
            initial_retry_interval: None,
            override_protocol_version: None,
            http_headers: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn a_credentials_only_context_names_the_server_it_has_no_endpoint_for() {
        let ctx = McpContext::credentials_only("x".to_string(), backend(), path());
        let err = ctx.endpoint().expect_err("no endpoint");
        assert!(err.to_string().contains('x'), "{err}");
    }

    #[test]
    fn an_endpoint_context_hands_its_endpoint_back() {
        let ctx = McpContext::with_endpoint("x".to_string(), backend(), path(), profile());
        assert!(ctx.endpoint().is_ok());
    }
}
