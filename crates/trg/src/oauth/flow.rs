//! Interactive OAuth 2.1 authorization-code + PKCE flow.

mod constants;
use constants::TEMPLATE;
use std::io::{stderr, stdin, IsTerminal};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use reqwest::Url;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession, OAuthTokenResponse, StoredCredentials,
};
use tiny_http::{Header, Response, Server};
use tracing::field::Empty;
use tracing::{Instrument, Span};

use crate::oauth::telemetry::{auth_error_type, record_error, OAuthStep};
use crate::shell::quote_for_shell;
use crate::telemetry::semconv::trg::{MCP_SERVER_NAME, OAUTH_CALLBACK_OUTCOME};

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error(
        "stdin/stderr is not a TTY; OAuth requires an interactive browser session\n\
         run `trg mcp auth login --server {}` once from a terminal, then try again",
        quote_for_shell(.server)
    )]
    NotATerminal { server: String },

    #[error("failed to bind a loopback port: {0}")]
    BindFailed(#[source] std::io::Error),

    #[error("failed to launch a browser at {url}: {source}\nopen this URL manually:\n  {url}")]
    BrowserOpenFailed {
        url: String,
        #[source]
        source: std::io::Error,
    },

    #[error("timed out waiting for the OAuth callback after {0:?}")]
    CallbackTimeout(std::time::Duration),

    #[error("authorization provider returned an error: {error}{}", description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    Provider { error: String, description: Option<String> },

    #[error("OAuth state mismatch (csrf protection): expected `{expected}`, got `{got}`")]
    StateMismatch { expected: String, got: String },

    #[error(transparent)]
    Oauth(#[from] AuthError),
}

impl FlowError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::NotATerminal { .. } => "not_a_terminal",
            Self::BindFailed(_) => "bind_failed",
            Self::BrowserOpenFailed { .. } => "browser_open_failed",
            Self::CallbackTimeout(_) => "callback_timeout",
            Self::Provider { .. } => "provider_error",
            Self::StateMismatch { .. } => "state_mismatch",
            Self::Oauth(error) => auth_error_type(error),
        }
    }
}

/// Render a server name for the recovery command so it survives a copy-paste into
/// a shell. Config keys are arbitrary strings, so a name can carry spaces or shell
/// metacharacters that would otherwise split it into several arguments.
pub struct FlowConfig {
    /// Default 5 minutes. Override for tests.
    pub callback_timeout: std::time::Duration,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            callback_timeout: Duration::from_secs(300),
        }
    }
}

/// Drives the interactive OAuth flow: loopback redirect, browser launch, and callback handling.
/// Token exchange uses [`AuthorizationSession::handle_callback`] (rmcp `AuthorizationManager::exchange_code_for_token`).
pub async fn run_authorization(
    auth_manager: AuthorizationManager,
    server_name: &str,
    scopes: &[&str],
    config: FlowConfig,
) -> Result<StoredCredentials, FlowError> {
    let span = OAuthStep::Authorize.span(server_name);
    let result = authorize(auth_manager, server_name, scopes, config)
        .instrument(span.clone())
        .await;
    if let Err(error) = &result {
        record_error(&span, error.error_type());
    }
    result
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
async fn authorize(
    auth_manager: AuthorizationManager,
    server_name: &str,
    scopes: &[&str],
    config: FlowConfig,
) -> Result<StoredCredentials, FlowError> {
    if !stdin().is_terminal() || !stderr().is_terminal() {
        return Err(FlowError::NotATerminal {
            server: server_name.to_string(),
        });
    }

    let server = Server::http("127.0.0.1:0").map_err(|e| FlowError::BindFailed(boxed_error_to_io(e)))?;
    let server = Arc::new(server);

    let port = loopback_port(server.as_ref())?;
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");

    let request = AuthorizationRequest::new(&redirect_uri).with_scopes(scopes.iter().copied());
    let session = {
        let register = OAuthStep::RegisterClient.span(server_name);
        AuthorizationSession::new(auth_manager, request)
            .instrument(register.clone())
            .await
            .map_err(|(_, e)| {
                record_error(&register, auth_error_type(&e));
                FlowError::Oauth(e)
            })?
    };

    let auth_url = session.get_authorization_url().to_string();
    let expected_state = oauth_state_from_authorization_url(&auth_url)
        .ok_or_else(|| AuthError::InternalError("authorization URL missing state parameter".to_string()))?;
    eprintln!("OAuth: open this URL in your browser if it doesn't open automatically: {auth_url}");

    let browser_err = open::that(&auth_url).err();

    let wait_outcome = {
        let wait = callback_wait_span(server_name);
        let server = server.clone();
        let timeout = config.callback_timeout;
        let expected_state = expected_state.clone();
        let server_name = server_name.to_string();
        let outcome =
            tokio::task::spawn_blocking(move || wait_for_callback(server, timeout, expected_state, server_name))
                .instrument(wait.clone())
                .await
                .map_err(|e| {
                    record_error(&wait, "callback_task_failed");
                    AuthError::InternalError(format!("OAuth callback task failed to run: {e}"))
                })?;
        outcome.record_on(&wait);
        outcome
    };

    let (code, state) = match wait_outcome {
        CallbackWait::Success { code, state } => (code, state),
        CallbackWait::Timeout => {
            if let Some(source) = browser_err {
                return Err(FlowError::BrowserOpenFailed { url: auth_url, source });
            }
            return Err(FlowError::CallbackTimeout(config.callback_timeout));
        }
        CallbackWait::StateMismatch { expected, got } => {
            return Err(FlowError::StateMismatch { expected, got });
        }
        CallbackWait::Provider { error, description } => {
            return Err(FlowError::Provider { error, description });
        }
    };

    let token_result = {
        let exchange = OAuthStep::ExchangeCode.span(server_name);
        session
            .handle_callback(&code, &state)
            .instrument(exchange.clone())
            .await
            .inspect_err(|e| record_error(&exchange, auth_error_type(e)))?
    };

    let granted_scopes = granted_scopes_from_token_response(&token_result);

    let (client_id, _) = session.get_credentials().await?;

    let token_received_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs());

    Ok(StoredCredentials::new(
        client_id,
        Some(token_result),
        granted_scopes,
        token_received_at,
    ))
}

fn loopback_port(server: &Server) -> Result<u16, FlowError> {
    match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => Ok(addr.port()),
        #[cfg(unix)]
        tiny_http::ListenAddr::Unix(_) => Err(FlowError::BindFailed(std::io::Error::other(
            "unexpected unix socket listen address",
        ))),
    }
}

fn granted_scopes_from_token_response(token: &OAuthTokenResponse) -> Vec<String> {
    let Ok(value) = serde_json::to_value(token) else {
        return Vec::new();
    };
    let Some(scope) = value.get("scope").and_then(|s| s.as_str()) else {
        return Vec::new();
    };
    scope.split_whitespace().map(str::to_string).collect()
}

fn oauth_state_from_authorization_url(auth_url: &str) -> Option<String> {
    let url = Url::parse(auth_url).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
}

#[derive(Debug)]
enum ParsedOAuthCallback {
    Success { code: String, state: String },
    Provider { error: String, description: Option<String> },
}

#[derive(Debug)]
enum CallbackPathError {
    WrongPath,
    Incomplete,
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "Malformed callback URLs are classified as incomplete protocol input without retaining or disclosing authorization URL details"
    )
)]
fn parse_oauth_callback_url(url_input: &str) -> Result<ParsedOAuthCallback, CallbackPathError> {
    let absolute = if url_input.starts_with("http://") || url_input.starts_with("https://") {
        url_input.to_string()
    } else {
        format!("http://127.0.0.1{url_input}")
    };
    let url = Url::parse(&absolute).map_err(|_| CallbackPathError::Incomplete)?;
    if url.path() != "/oauth/callback" {
        return Err(CallbackPathError::WrongPath);
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error_description = Some(v.into_owned()),
            _ => {}
        }
    }

    if error.is_some() || error_description.is_some() {
        return Ok(ParsedOAuthCallback::Provider {
            error: error.unwrap_or_default(),
            description: error_description,
        });
    }

    match (code, state) {
        (Some(code), Some(state)) => Ok(ParsedOAuthCallback::Success { code, state }),
        _ => Err(CallbackPathError::Incomplete),
    }
}

enum CallbackWait {
    Success { code: String, state: String },
    StateMismatch { expected: String, got: String },
    Provider { error: String, description: Option<String> },
    Timeout,
}

impl CallbackWait {
    fn outcome(&self) -> &'static str {
        match self {
            Self::Success { .. } => "received",
            Self::StateMismatch { .. } => "state_mismatch",
            Self::Provider { .. } => "provider_error",
            Self::Timeout => "timeout",
        }
    }

    fn record_on(&self, span: &Span) {
        span.record(OAUTH_CALLBACK_OUTCOME, self.outcome());
        if !matches!(self, Self::Success { .. }) {
            record_error(span, self.outcome());
        }
    }
}

/// The wait is a person switching to a browser and signing in, so its
/// duration measures them, not `trg`.
fn callback_wait_span(server_name: &str) -> Span {
    tracing::info_span!(
        "wait oauth_callback",
        "otel.status_code" = Empty,
        { MCP_SERVER_NAME } = server_name,
        { OAUTH_CALLBACK_OUTCOME } = Empty,
        { ERROR_TYPE } = Empty,
    )
}

struct UnblockServer(Arc<Server>);

impl Drop for UnblockServer {
    fn drop(&mut self) {
        self.0.unblock();
    }
}

fn wait_for_callback(
    server: Arc<Server>,
    overall_timeout: Duration,
    expected_state: String,
    server_name: String,
) -> CallbackWait {
    let _cleanup = UnblockServer(server.clone());
    let start = Instant::now();

    loop {
        let elapsed = start.elapsed();
        if elapsed >= overall_timeout {
            return CallbackWait::Timeout;
        }
        let remaining = overall_timeout - elapsed;
        let request = match server.recv_timeout(remaining) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => continue,
        };

        let url_str = request.url();
        match parse_oauth_callback_url(url_str) {
            Ok(ParsedOAuthCallback::Success { code, state }) if state == expected_state => {
                let _ = request.respond(success_response(&server_name));
                return CallbackWait::Success { code, state };
            }
            Ok(ParsedOAuthCallback::Success { state, .. }) => {
                let _ = request.respond(bad_request_response());
                return CallbackWait::StateMismatch {
                    expected: expected_state.clone(),
                    got: state,
                };
            }
            Ok(ParsedOAuthCallback::Provider { error, description }) => {
                let _ = request.respond(provider_error_response());
                return CallbackWait::Provider { error, description };
            }
            Err(CallbackPathError::WrongPath) => {
                let _ = request.respond(not_found_response());
            }
            Err(CallbackPathError::Incomplete) => {
                let _ = request.respond(bad_request_response());
            }
        }
    }
}

/// The page shown once the provider redirects back with a matching state.
///
/// It cannot say the credential was stored, because at this point none exists:
/// the code has not been exchanged yet, and the write to the secrets backend
/// happens two calls further out in [`crate::oauth::ensure_credentials_for`].
/// Either can still fail, and an expired OpenBao token makes that routine, so
/// a page claiming storage here would contradict the terminal on the failure
/// this whole flow is most likely to hit.
fn success_response(server_name: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    html_response(&page(
        "Authorized",
        Mark::Ok,
        "Authorized",
        &format!(
            "<p class=\"subject\"><code>{}</code></p>\n<p><code>trg</code> is finishing in your terminal. You can close this tab.</p>",
            escape_html(server_name)
        ),
    ))
}

fn provider_error_response() -> Response<std::io::Cursor<Vec<u8>>> {
    html_response(&page(
        "Authorization failed",
        Mark::Bad,
        "Authorization failed",
        "<p>Nothing was stored. Your terminal has the reason. You can close this tab.</p>",
    ))
}

enum Mark {
    Ok,
    Bad,
}

impl Mark {
    fn svg(&self) -> &'static str {
        match self {
            Mark::Ok => {
                r##"<svg class="mark" viewBox="0 0 44 44" fill="none" aria-hidden="true"><circle cx="22" cy="22" r="20" stroke="var(--ok)" stroke-width="2" opacity=".3"/><path d="M14 22.5l5.5 5.5L30 17" stroke="var(--ok)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/></svg>"##
            }
            Mark::Bad => {
                r##"<svg class="mark" viewBox="0 0 44 44" fill="none" aria-hidden="true"><circle cx="22" cy="22" r="20" stroke="var(--bad)" stroke-width="2" opacity=".3"/><path d="M16.5 16.5l11 11M27.5 16.5l-11 11" stroke="var(--bad)" stroke-width="2.5" stroke-linecap="round"/></svg>"##
            }
        }
    }
}

/// The page a browser lands on when the flow ends.
///
/// The markup is a file rather than a string literal so the CSS keeps its own
/// braces and an editor can highlight it. `include_str!` reads it at compile
/// time, so the binary still ships alone.
///
/// Everything in it is inline: no fonts, no scripts, no images fetched. A page
/// served by this crate that reached off the machine would turn every login
/// into a request some third party could count, and it would break on the
/// air-gapped hosts that are the reason `token` exists alongside `token_file`.
///
/// `body` is substituted as markup, so anything from outside this function has
/// to go through [`escape_html`] before it gets here.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        literal_template_replace,
        reason = "The bundled HTML template contains CSS braces and named markup slots, so format! cannot parse it as a Rust format string"
    )
)]
fn page(title: &str, mark: Mark, heading: &str, body: &str) -> String {
    TEMPLATE
        .replace("{{TITLE}}", title)
        .replace("{{MARK}}", mark.svg())
        .replace("{{HEADING}}", heading)
        .replace("{{BODY}}", body)
}

/// Escape text for interpolation into [`page`].
///
/// A server name reaches this page, and the Keychain backend accepts any name
/// at all, so one containing markup would otherwise be rendered as markup.
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn html_response(body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body);
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]) {
        r = r.with_header(h);
    }
    r
}

fn not_found_response() -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string("Not Found").with_status_code(404);
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..]) {
        r = r.with_header(h);
    }
    r
}

fn bad_request_response() -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string("Bad Request").with_status_code(400);
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..]) {
        r = r.with_header(h);
    }
    r
}

fn boxed_error_to_io(err: Box<dyn std::error::Error + Send + Sync>) -> std::io::Error {
    match err.downcast::<std::io::Error>() {
        Ok(io) => *io,
        Err(err) => std::io::Error::other(err),
    }
}

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
