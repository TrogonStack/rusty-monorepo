//! Interactive OAuth 2.1 authorization-code + PKCE flow.

use std::io::{stderr, stdin, IsTerminal};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Url;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession, OAuthTokenResponse, StoredCredentials,
};
use tiny_http::{Header, Response, Server};

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

/// Render a server name for the recovery command so it survives a copy-paste into
/// a shell. Config keys are arbitrary strings, so a name can carry spaces or shell
/// metacharacters that would otherwise split it into several arguments.
pub(crate) fn quote_for_shell(name: &str) -> String {
    let is_bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | '+' | '=' | ','));

    if is_bare {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', r"'\''"))
    }
}

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
    let session = AuthorizationSession::new(auth_manager, request)
        .await
        .map_err(|(_, e)| FlowError::Oauth(e))?;

    let auth_url = session.get_authorization_url().to_string();
    let expected_state = oauth_state_from_authorization_url(&auth_url)
        .ok_or_else(|| AuthError::InternalError("authorization URL missing state parameter".to_string()))?;
    eprintln!("OAuth: open this URL in your browser if it doesn't open automatically: {auth_url}");

    let browser_err = open::that(&auth_url).err();

    let wait_outcome = {
        let server = server.clone();
        let timeout = config.callback_timeout;
        let expected_state = expected_state.clone();
        let server_name = server_name.to_string();
        tokio::task::spawn_blocking(move || wait_for_callback(server, timeout, expected_state, server_name))
            .await
            .map_err(|e| AuthError::InternalError(format!("OAuth callback task failed to run: {e}")))?
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

    let token_result = session.handle_callback(&code, &state).await?;

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

fn success_response(server_name: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    html_response(&page(
        "Authorized",
        Mark::Ok,
        "Authorized",
        &format!(
            "<p class=\"subject\"><code>{}</code></p>\n<p><code>trg</code> stored the credential. You can close this tab.</p>",
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
/// Everything is inline: no fonts, no scripts, no images fetched. A page served
/// by this crate that reached off the machine would turn every login into a
/// request some third party could count, and it would break on the air-gapped
/// hosts that are the reason `token` exists alongside `token_file`.
///
/// `body` is interpolated as markup, so anything from outside this function has
/// to go through [`escape_html`] before it gets here.
fn page(title: &str, mark: Mark, heading: &str, body: &str) -> String {
    let mark = mark.svg();
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{title} &middot; trg</title>
<style>
:root {{
  --bg:#f6f7f9; --card:#fff; --line:#e4e7eb; --fg:#11151b; --muted:#5b6672;
  --chip:rgba(17,21,27,.06); --ok:#1a7f37; --bad:#b3261e;
  --shadow:0 1px 2px rgba(17,21,27,.05),0 10px 30px rgba(17,21,27,.08);
}}
@media (prefers-color-scheme:dark) {{
  :root {{
    --bg:#0b0d10; --card:#14171c; --line:#232831; --fg:#e6e9ed; --muted:#98a1ad;
    --chip:rgba(230,233,237,.08); --ok:#3fb950; --bad:#f85149;
    --shadow:0 1px 2px rgba(0,0,0,.4),0 10px 30px rgba(0,0,0,.35);
  }}
}}
*{{box-sizing:border-box}}
html,body{{height:100%}}
body{{
  margin:0; display:grid; place-items:center; padding:24px;
  background:var(--bg); color:var(--fg);
  font:15px/1.55 ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
  -webkit-font-smoothing:antialiased;
}}
.card{{
  width:100%; max-width:27rem; text-align:center;
  background:var(--card); border:1px solid var(--line); border-radius:14px;
  box-shadow:var(--shadow); padding:34px 32px 18px;
}}
.mark{{width:44px;height:44px}}
h1{{margin:18px 0 10px;font-size:1.2rem;font-weight:600;letter-spacing:-.01em}}
p{{margin:0;color:var(--muted)}}
.subject{{margin:0 0 12px}}
.subject code{{font-size:13.5px;padding:.28em .6em}}
code{{
  font:13px/1.4 ui-monospace,SFMono-Regular,"SF Mono",Menlo,Consolas,monospace;
  background:var(--chip); border-radius:5px; padding:.12em .4em;
}}
footer{{
  margin-top:28px; padding-top:16px; border-top:1px solid var(--line);
  display:flex; align-items:center; justify-content:center; gap:8px;
  font-size:12.5px; color:var(--muted);
}}
footer a{{color:inherit;text-decoration:none}}
footer a:hover{{color:var(--fg);text-decoration:underline}}
.wordmark{{font-weight:600;color:var(--fg)}}
.sep{{opacity:.45}}
</style>
</head>
<body>
<main class="card">
{mark}
<h1>{heading}</h1>
{body}
<footer>
<span class="wordmark">TrogonStack</span>
<span class="sep">&middot;</span>
<a href="https://github.com/TrogonStack">github.com/TrogonStack</a>
</footer>
</main>
</body>
</html>
"##
    )
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
mod tests {
    use super::*;

    /// The Keychain backend accepts any server name, so one carrying markup
    /// reaches this page. It has to arrive as text.
    #[test]
    fn a_server_name_is_escaped_into_the_page() {
        let body = read_body(success_response("<script>alert(1)</script>"));

        assert!(!body.contains("<script>"), "{body}");
        assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{body}");
    }

    #[test]
    fn the_success_page_names_the_server_that_was_authorized() {
        let body = read_body(success_response("linear"));

        assert!(body.contains("Authorized"), "{body}");
        assert!(body.contains("<code>linear</code>"), "{body}");
    }

    /// A page served off a loopback port during a login must not turn that
    /// login into a request anyone else can observe, and must render on a host
    /// with no route out.
    #[test]
    fn the_pages_fetch_nothing_but_the_one_link_they_offer() {
        for body in [
            read_body(success_response("linear")),
            read_body(provider_error_response()),
        ] {
            assert!(!body.contains("<script"), "{body}");
            for attr in ["src=", "@import", "url("] {
                assert!(!body.contains(attr), "found {attr} in {body}");
            }
            assert_eq!(body.matches("https://").count(), 1, "{body}");
            assert!(body.contains("https://github.com/TrogonStack"), "{body}");
        }
    }

    #[test]
    fn the_failure_page_says_nothing_was_kept() {
        let body = read_body(provider_error_response());

        assert!(body.contains("Authorization failed"), "{body}");
        assert!(body.contains("Nothing was stored"), "{body}");
    }

    fn read_body(response: Response<std::io::Cursor<Vec<u8>>>) -> String {
        let mut out = Vec::new();
        response
            .raw_print(&mut out, (1, 1).into(), &[], false, None)
            .expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn flow_config_default_callback_timeout_is_300_seconds() {
        assert_eq!(FlowConfig::default().callback_timeout, Duration::from_secs(300));
    }

    #[test]
    fn parses_code_and_state_from_full_callback_url() {
        let parsed = parse_oauth_callback_url("http://127.0.0.1:9999/oauth/callback?code=abc&state=xyz");
        match parsed {
            Ok(ParsedOAuthCallback::Success { code, state }) => {
                assert_eq!(code, "abc");
                assert_eq!(state, "xyz");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_provider_error_query() {
        let parsed = parse_oauth_callback_url(
            "http://127.0.0.1:9999/oauth/callback?error=access_denied&error_description=user+rejected",
        );
        match parsed {
            Ok(ParsedOAuthCallback::Provider { error, description }) => {
                assert_eq!(error, "access_denied");
                assert_eq!(description.as_deref(), Some("user rejected"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn flow_error_display_not_a_terminal() {
        assert_eq!(
            FlowError::NotATerminal {
                server: "exa".to_string(),
            }
            .to_string(),
            "stdin/stderr is not a TTY; OAuth requires an interactive browser session\n\
             run `trg mcp auth login --server exa` once from a terminal, then try again"
        );
    }

    #[test]
    fn flow_error_display_not_a_terminal_quotes_awkward_server_names() {
        assert_eq!(
            FlowError::NotATerminal {
                server: "my server".to_string(),
            }
            .to_string(),
            "stdin/stderr is not a TTY; OAuth requires an interactive browser session\n\
             run `trg mcp auth login --server 'my server'` once from a terminal, then try again"
        );
    }

    #[test]
    fn quote_for_shell_leaves_bare_names_alone() {
        assert_eq!(quote_for_shell("exa"), "exa");
        assert_eq!(quote_for_shell("my-server_2.0"), "my-server_2.0");
    }

    #[test]
    fn quote_for_shell_wraps_names_needing_it() {
        assert_eq!(quote_for_shell(""), "''");
        assert_eq!(quote_for_shell("my server"), "'my server'");
        assert_eq!(quote_for_shell("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(quote_for_shell("it's"), r"'it'\''s'");
    }

    #[test]
    fn flow_error_display_callback_timeout() {
        let msg = FlowError::CallbackTimeout(Duration::from_secs(300)).to_string();
        assert!(msg.contains("timed out waiting for the OAuth callback"), "{msg}");
        assert!(msg.contains("300s"), "{msg}");
    }

    #[test]
    fn flow_error_display_provider_with_description() {
        let err = FlowError::Provider {
            error: "access_denied".to_string(),
            description: Some("user rejected".to_string()),
        };
        assert_eq!(
            err.to_string(),
            "authorization provider returned an error: access_denied: user rejected"
        );
    }

    #[test]
    fn flow_error_display_provider_without_description() {
        let err = FlowError::Provider {
            error: "access_denied".to_string(),
            description: None,
        };
        assert_eq!(
            err.to_string(),
            "authorization provider returned an error: access_denied"
        );
    }

    #[test]
    fn flow_error_display_state_mismatch() {
        let err = FlowError::StateMismatch {
            expected: "exp".to_string(),
            got: "got".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "OAuth state mismatch (csrf protection): expected `exp`, got `got`"
        );
    }

    #[test]
    fn flow_error_display_browser_open_failed() {
        let source = std::io::Error::other("no browser");
        let err = FlowError::BrowserOpenFailed {
            url: "http://example/oauth".to_string(),
            source,
        };
        assert_eq!(
            err.to_string(),
            "failed to launch a browser at http://example/oauth: no browser\nopen this URL manually:\n  http://example/oauth"
        );
    }
}
