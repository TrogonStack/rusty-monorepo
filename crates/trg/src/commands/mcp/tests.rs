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
    let ctx = McpContext::credentials_only("x".to_string(), backend(), path(), None);
    let err = ctx.endpoint().expect_err("no endpoint");
    assert!(err.to_string().contains('x'), "{err}");
}

#[test]
fn an_endpoint_context_hands_its_endpoint_back() {
    let ctx = McpContext::with_endpoint("x".to_string(), backend(), path(), None, profile());
    assert!(ctx.endpoint().is_ok());
}
