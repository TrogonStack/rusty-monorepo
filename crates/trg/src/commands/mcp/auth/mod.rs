//! `trg mcp auth`: manage the OAuth credentials of a configured MCP server.

use clap::{Args, Subcommand};
use oauth2::TokenResponse;
use rmcp::transport::auth::{CredentialStore, OAuthTokenResponse, StoredCredentials};
use serde::Serialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::mcp::McpContext;
use crate::exit_code::ExitCode;
use crate::oauth::store::OAuthCredentialStore;
use crate::oauth::telemetry::OAuthHttp;
use crate::oauth::{ensure_credentials_for, EnsureError, EnsureOutcome};
use crate::output::{print_json, OutputFormat};
use crate::secrets::SecretPath;
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
    /// Present only when the load fell through to the pre-`machine_id` shared
    /// path. A consumer that assumes the configured path is where these live
    /// would otherwise be as wrong as the text output used to be.
    #[serde(skip_serializing_if = "Option::is_none")]
    read_from_shared_path: Option<&'a str>,
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
    fn from(
        stored: &'a StoredCredentials,
        summary: Option<&'a TokenSummary>,
        read_from: Option<&'a SecretPath>,
    ) -> Self {
        Self {
            client_id: &stored.client_id,
            granted_scopes: &stored.granted_scopes,
            read_from_shared_path: read_from.map(SecretPath::as_str),
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

    /// Output format. The authorize URL and the browser prompt go to stderr
    /// under either one; `json` prints only the outcome of the flow.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Server name as it appears under `[mcp.servers.<name>]`.
    #[arg(long)]
    pub server: String,

    /// Output format. Neither format emits a token; `json` prints the same
    /// redacted summary the text form describes.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

#[derive(Args, Debug, Clone)]
pub struct LogoutArgs {
    /// Server name as it appears under `[mcp.servers.<name>]`.
    #[arg(long)]
    pub server: String,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
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

    /// `status` and `logout` read and delete what is already stored, so they
    /// never reach the endpoint and must keep working when resolving one would
    /// not.
    pub fn needs_endpoint(&self) -> bool {
        match self {
            AuthCommands::Login(_) => true,
            AuthCommands::Status(_) | AuthCommands::Logout(_) => false,
        }
    }

    pub async fn handle(self, ctx: &McpContext) -> ExitCode {
        match self {
            AuthCommands::Login(args) => match login(args.output_format, ctx).await {
                Ok(code) => code,
                Err(e) => emit(e),
            },
            AuthCommands::Status(args) => match status(args.output_format, ctx).await {
                Ok(code) => code,
                Err(e) => emit(e),
            },
            AuthCommands::Logout(args) => match logout(ctx).await {
                Ok(still_reachable) => cleared(args.output_format, ctx, still_reachable.as_ref()),
                Err(e) => emit(e),
            },
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
fn emit<E: std::fmt::Display>(e: E) -> ExitCode {
    eprintln!("{e}");
    ExitCode::Failed
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
async fn login(format: OutputFormat, ctx: &McpContext) -> Result<ExitCode, AuthError> {
    let server = ctx.server_name.as_str();
    let where_stored = ctx.backend.describe();
    // No fallback: an explicit login is re-authorizing this machine, so a
    // credential still sitting at the shared path must not count as already
    // being authorized, or the flow never runs and `cred_path` never gets
    // written.
    let http = OAuthHttp::traced().map_err(EnsureError::from)?;
    let outcome = ensure_credentials_for(ctx.endpoint()?, server, &ctx.backend, &ctx.cred_path, None, &http).await?;

    if format.is_json() {
        #[derive(Serialize)]
        struct LoginDocument<'a> {
            server: &'a str,
            outcome: &'static str,
            backend: String,
            path: String,
        }
        let document = LoginDocument {
            server,
            outcome: match outcome {
                EnsureOutcome::NoAuthRequired => "no_auth_required",
                EnsureOutcome::AlreadyAuthorized(_) => "already_authorized",
                EnsureOutcome::Authorized(_) => "authorized",
            },
            backend: where_stored,
            path: ctx.cred_path.to_string(),
        };
        return Ok(print_json(&document, ExitCode::Success));
    }

    match outcome {
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
    Ok(ExitCode::Success)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
async fn status(format: OutputFormat, ctx: &McpContext) -> Result<ExitCode, AuthError> {
    let server = ctx.server_name.as_str();
    let store = OAuthCredentialStore::new(
        ctx.backend.clone(),
        ctx.cred_path.clone(),
        &ctx.server_name,
        ctx.fallback.clone(),
    );
    let read_from = store.read_from();

    let Some(stored) = store.load().await? else {
        if format.is_json() {
            #[derive(Serialize)]
            struct AbsentCredentials<'a> {
                server: &'a str,
                stored: bool,
            }
            return Ok(print_json(
                &AbsentCredentials { server, stored: false },
                ExitCode::Success,
            ));
        }
        println!("No OAuth credentials stored for `{server}`.");
        return Ok(ExitCode::Success);
    };

    let summary = stored.token_response.as_ref().map(TokenSummary::from_token);
    let read_from = read_from.get();

    if format.is_json() {
        return Ok(print_json(
            &StoredCredentialsView::from(&stored, summary.as_ref(), read_from.as_ref()),
            ExitCode::Success,
        ));
    }

    print_summary(ctx, &stored, summary.as_ref(), read_from.as_ref());
    Ok(ExitCode::Success)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn print_summary(
    ctx: &McpContext,
    stored: &StoredCredentials,
    summary: Option<&TokenSummary>,
    read_from: Option<&SecretPath>,
) {
    println!("Server:       {}", ctx.server_name);
    println!("Backend:      {}", ctx.backend.describe());
    match read_from {
        // The load fell through to the pre-machine_id shared path: printing
        // `ctx.cred_path` here would claim credentials live somewhere they
        // don't yet.
        Some(shared) => println!(
            "Path:         {shared} (shared; the machine-scoped path `{}` is empty until you re-authorize)",
            ctx.cred_path
        ),
        None => println!("Path:         {}", ctx.cred_path),
    }
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

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn print_token_summary(summary: &TokenSummary, received_at: u64) {
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

/// Deletes the machine-scoped credential and reports whether this machine is
/// still effectively signed in afterward. `clear` only ever touches
/// `ctx.cred_path`, since deleting the shared path would sign out every
/// other machine reading from it, so a credential can still be reachable
/// through the fallback once this returns. The `Some(path)` case names that
/// shared path, for `cleared` to report honestly instead of claiming a clean
/// logout.
///
/// Checks the fallback strictly, not through `load`: a storage error while
/// checking has to surface rather than read as "nothing there", or this
/// reports the clean logout it just stopped lying about.
async fn logout(ctx: &McpContext) -> Result<Option<SecretPath>, AuthError> {
    let store = OAuthCredentialStore::new(
        ctx.backend.clone(),
        ctx.cred_path.clone(),
        &ctx.server_name,
        ctx.fallback.clone(),
    );
    store.clear().await?;
    Ok(store.fallback_is_reachable().await?)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn cleared(format: OutputFormat, ctx: &McpContext, still_reachable_via: Option<&SecretPath>) -> ExitCode {
    let removal_command = still_reachable_via.and_then(|shared| ctx.backend.removal_command(shared));

    if format.is_json() {
        return print_json(
            &cleared_document(&ctx.server_name, still_reachable_via, removal_command.as_deref()),
            ExitCode::Success,
        );
    }

    println!(
        "{}",
        cleared_message(&ctx.server_name, still_reachable_via, removal_command.as_deref())
    );
    ExitCode::Success
}

/// The JSON shape for `logout`. `cleared: true` alone would claim this
/// machine is fully signed out even when `still_reachable_via` names a
/// fallback it will keep authenticating from, so that case adds fields
/// saying so instead of standing on its own.
///
/// `removal_command` is fully qualified by the backend that owns the path
/// layout, never reconstructed here: a `SecretPath` is backend-relative, so
/// printing it alone as a command would name something that cannot be run,
/// or worse something else entirely at that relative path under a different
/// mount. `None` when the backend has no such command to give.
fn cleared_document(server: &str, still_reachable_via: Option<&SecretPath>, removal_command: Option<&str>) -> Value {
    #[derive(Serialize)]
    struct ClearedDocument<'a> {
        server: &'a str,
        cleared: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        shared_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    }
    let document = match still_reachable_via {
        Some(shared) => {
            let note = match removal_command {
                Some(command) => format!(
                    "this machine will keep authenticating from the shared credential at `{shared}`; \
                     remove it with `{command}` to sign out every machine using it"
                ),
                None => format!(
                    "this machine will keep authenticating from the shared credential at `{shared}`; \
                     remove it directly against the backend to sign out every machine using it"
                ),
            };
            ClearedDocument {
                server,
                cleared: true,
                shared_path: Some(shared.to_string()),
                note: Some(note),
            }
        }
        None => ClearedDocument {
            server,
            cleared: true,
            shared_path: None,
            note: None,
        },
    };
    serde_json::to_value(document).expect("cleared document contains only strings and a boolean")
}

fn cleared_message(server: &str, still_reachable_via: Option<&SecretPath>, removal_command: Option<&str>) -> String {
    match still_reachable_via {
        Some(shared) => match removal_command {
            Some(command) => format!(
                "Machine-scoped OAuth credentials cleared for `{server}`, but a shared credential at `{shared}` \
                 is still reachable: this machine will keep authenticating from it. Remove it with `{command}` \
                 to sign out every machine using it."
            ),
            None => format!(
                "Machine-scoped OAuth credentials cleared for `{server}`, but a shared credential at `{shared}` \
                 is still reachable: this machine will keep authenticating from it. Remove it directly against \
                 the backend to sign out every machine using it."
            ),
        },
        None => format!("OAuth credentials cleared for `{server}`."),
    }
}

#[cfg(test)]
mod tests;
