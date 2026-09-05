//! `trg mcp auth`: manage the OAuth credentials of a configured MCP server.

use clap::{Args, Subcommand, ValueEnum};
use oauth2::TokenResponse;
use rmcp::transport::auth::{CredentialStore, OAuthTokenResponse, StoredCredentials};
use serde::Serialize;
use serde_json::{json, Value};

use crate::commands::mcp::McpContext;
use crate::oauth::{ensure_credentials_for, store::OAuthCredentialStore, EnsureError, EnsureOutcome};
use crate::term;

/// Display view over an `OAuthTokenResponse`.
///
/// `access_token` and `token_type` are always present per RFC 6749; the rest
/// are AS-dependent. Extra fields (RFC 8707 `resource`, `id_token`) come from
/// rmcp's `VendorExtraTokenFields` which exposes its inner `HashMap` directly,
/// so no serde round-trip is needed.
#[derive(Debug)]
struct TokenSummary {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: String,
    expires_in: Option<u64>,
    scope: Option<String>,
    resource: Option<String>,
}

impl TokenSummary {
    fn from_token(token: &OAuthTokenResponse) -> Self {
        let extras = &token.extra_fields().0;
        let extra_str = |key: &str| extras.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            access_token: token.access_token().secret().clone(),
            refresh_token: token.refresh_token().map(|t| t.secret().clone()),
            id_token: extra_str("id_token"),
            token_type: token.token_type().as_ref().to_string(),
            expires_in: token.expires_in().map(|d| d.as_secs()),
            scope: token
                .scopes()
                .map(|scopes| scopes.iter().map(|s| s.as_ref()).collect::<Vec<&str>>().join(" ")),
            resource: extra_str("resource"),
        }
    }
}

#[derive(Serialize)]
struct StoredCredentialsView<'a> {
    client_id: &'a str,
    granted_scopes: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    token_received_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_response: Option<TokenResponseView<'a>>,
}

#[derive(Serialize)]
struct TokenResponseView<'a> {
    token_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
    access_token: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_token: Option<&'static str>,
}

impl<'a> StoredCredentialsView<'a> {
    fn from(stored: &'a StoredCredentials, summary: Option<&'a TokenSummary>) -> Self {
        Self {
            client_id: &stored.client_id,
            granted_scopes: &stored.granted_scopes,
            token_received_at: stored.token_received_at,
            token_response: summary.map(|s| TokenResponseView {
                token_type: &s.token_type,
                expires_in: s.expires_in,
                scope: s.scope.as_deref(),
                resource: s.resource.as_deref(),
                access_token: "<redacted>",
                refresh_token: s.refresh_token.as_deref().map(|_| "<redacted>"),
                id_token: s.id_token.as_deref().map(|_| "<redacted>"),
            }),
        }
    }
}

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Run the interactive OAuth flow for a configured MCP server and exit.
    Login(LoginArgs),

    /// Print the stored OAuth credential summary for a configured MCP server.
    Status(StatusArgs),

    /// Delete the cached OAuth credentials for a configured MCP server.
    Logout(LogoutArgs),
}

#[derive(Args, Debug, Clone)]
pub struct LoginArgs {
    /// Server name as it appears under `[mcp.servers.<name>]`.
    #[arg(long)]
    pub server: String,
}

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Server name as it appears under `[mcp.servers.<name>]`.
    #[arg(long)]
    pub server: String,

    /// Output format. `json` prints a redacted credential summary (tokens are not emitted).
    #[arg(long, value_enum, default_value_t = StatusFormat::Text)]
    pub format: StatusFormat,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum StatusFormat {
    Text,
    Json,
}

#[derive(Args, Debug, Clone)]
pub struct LogoutArgs {
    /// Server name as it appears under `[mcp.servers.<name>]`.
    #[arg(long)]
    pub server: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{0}")]
    Ensure(#[from] EnsureError),

    #[error("OAuth: {0}")]
    Store(#[from] rmcp::transport::auth::AuthError),
}

impl AuthCommands {
    pub fn server_name(&self) -> &str {
        match self {
            AuthCommands::Login(args) => args.server.trim(),
            AuthCommands::Status(args) => args.server.trim(),
            AuthCommands::Logout(args) => args.server.trim(),
        }
    }

    pub async fn handle(self, ctx: &McpContext) -> i32 {
        match self {
            AuthCommands::Login(_) => match login(ctx).await {
                Ok(()) => 0,
                Err(e) => emit(e),
            },
            AuthCommands::Status(args) => match status(&args, ctx).await {
                Ok(()) => 0,
                Err(e) => emit(e),
            },
            AuthCommands::Logout(_) => match logout(ctx).await {
                Ok(()) => {
                    println!("OAuth credentials cleared for `{}`.", ctx.server_name);
                    0
                }
                Err(e) => emit(e),
            },
        }
    }
}

fn emit<E: std::fmt::Display>(e: E) -> i32 {
    eprintln!("{e}");
    1
}

async fn login(ctx: &McpContext) -> Result<(), AuthError> {
    let server = ctx.server_name.as_str();
    let where_stored = ctx.backend.describe();
    match ensure_credentials_for(&ctx.profile, server, &ctx.backend, &ctx.cred_path).await? {
        EnsureOutcome::NoAuthRequired => {
            println!(
                "`{server}` does not require OAuth (no discovery support, or static \
                 Authorization header configured)."
            );
        }
        EnsureOutcome::AlreadyAuthorized(_) => {
            println!(
                "OAuth credentials already cached for `{server}` in {where_stored}. \
                 Use `trg mcp auth logout --server {server}` to force re-auth."
            );
        }
        EnsureOutcome::Authorized(_) => {
            // Only this outcome reaches the browser, so only this one lands
            // directly under the authorize URL the flow wrote to stderr. The
            // other two short-circuit before that and need no separation.
            println!();
            println!(
                "{} Credentials stored in {where_stored} at `{}`.",
                term::green(&format!("OAuth complete for `{server}`.")),
                ctx.cred_path
            );
        }
    }
    Ok(())
}

async fn status(args: &StatusArgs, ctx: &McpContext) -> Result<(), AuthError> {
    let server = ctx.server_name.as_str();
    let store = OAuthCredentialStore::new(ctx.backend.clone(), ctx.cred_path.clone(), &ctx.server_name);

    let Some(stored) = store.load().await? else {
        match args.format {
            StatusFormat::Text => println!("No OAuth credentials stored for `{server}`."),
            StatusFormat::Json => println!("{}", json!({ "server": server, "stored": false })),
        }
        return Ok(());
    };

    let summary = stored.token_response.as_ref().map(TokenSummary::from_token);

    match args.format {
        StatusFormat::Json => {
            let view = StoredCredentialsView::from(&stored, summary.as_ref());
            let json = serde_json::to_string_pretty(&view).unwrap_or_else(|e| format!("<failed to serialize: {e}>"));
            println!("{json}");
        }
        StatusFormat::Text => print_summary(ctx, &stored, summary.as_ref()),
    }
    Ok(())
}

fn print_summary(ctx: &McpContext, stored: &StoredCredentials, summary: Option<&TokenSummary>) {
    println!("Server:       {}", ctx.server_name);
    println!("Backend:      {}", ctx.backend.describe());
    println!("Path:         {}", ctx.cred_path);
    println!("Client ID:    {}", stored.client_id);

    if stored.granted_scopes.is_empty() || (stored.granted_scopes.len() == 1 && stored.granted_scopes[0].is_empty()) {
        println!("Scopes:       <none granted>");
    } else {
        println!("Scopes:       {}", stored.granted_scopes.join(", "));
    }

    match (summary, stored.token_received_at) {
        (Some(s), Some(received_at)) => print_token_summary(s, received_at),
        (Some(_), None) => println!("Issued at:    <unknown>"),
        (None, _) => println!("Token:        <not present, re-auth required>"),
    }
}

fn print_token_summary(summary: &TokenSummary, received_at: u64) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let issued_ago = now.saturating_sub(received_at);
    println!(
        "Issued at:    epoch {received_at} ({} ago)",
        format_duration(issued_ago),
    );

    if let Some(expires_in) = summary.expires_in {
        let remaining = expires_in.saturating_sub(issued_ago);
        let suffix = if remaining == 0 { "expired" } else { "remaining" };
        println!(
            "Expires in:   {} from issued_at ({} {suffix})",
            format_duration(expires_in),
            format_duration(remaining),
        );
    } else {
        println!("Expires in:   <not advertised>");
    }

    println!("Token type:   {}", summary.token_type);
    if let Some(res) = &summary.resource {
        println!("Resource:     {res}");
    }
    println!("Access token: {}", present_chars(&summary.access_token));
    println!(
        "Refresh:      {}",
        summary
            .refresh_token
            .as_deref()
            .map(present_chars)
            .unwrap_or_else(|| "no".to_string()),
    );
}

fn present_chars(token: &str) -> String {
    format!("yes ({} chars)", token.chars().count())
}

fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return "0s".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 && days == 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && days == 0 && hours == 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

async fn logout(ctx: &McpContext) -> Result<(), AuthError> {
    let store = OAuthCredentialStore::new(ctx.backend.clone(), ctx.cred_path.clone(), &ctx.server_name);
    store.clear().await?;
    Ok(())
}
