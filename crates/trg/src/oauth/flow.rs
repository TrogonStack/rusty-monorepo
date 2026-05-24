//! Interactive OAuth 2.1 authorization-code + PKCE flow.

use std::io::{stderr, stdin, IsTerminal};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Url;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationSession, OAuthTokenResponse, StoredCredentials,
};
use tiny_http::{Header, Response, Server};

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error("stdin/stderr is not a TTY; OAuth requires an interactive browser session")]
    NotATerminal,

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
    scopes: &[&str],
    config: FlowConfig,
) -> Result<StoredCredentials, FlowError> {
    if !stdin().is_terminal() || !stderr().is_terminal() {
        return Err(FlowError::NotATerminal);
    }

    let server = Server::http("127.0.0.1:0").map_err(|e| FlowError::BindFailed(boxed_error_to_io(e)))?;
    let server = Arc::new(server);

    let port = loopback_port(server.as_ref())?;
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");

    let session = AuthorizationSession::new(auth_manager, scopes, &redirect_uri, None, None).await?;

    let auth_url = session.get_authorization_url().to_string();
    let expected_state = oauth_state_from_authorization_url(&auth_url)
        .ok_or_else(|| AuthError::InternalError("authorization URL missing state parameter".to_string()))?;
    eprintln!("OAuth: open this URL in your browser if it doesn't open automatically: {auth_url}");

    let browser_err = open::that(&auth_url).err();

    let wait_outcome = {
        let server = server.clone();
        let timeout = config.callback_timeout;
        let expected_state = expected_state.clone();
        tokio::task::spawn_blocking(move || wait_for_callback(server, timeout, expected_state))
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

fn wait_for_callback(server: Arc<Server>, overall_timeout: Duration, expected_state: String) -> CallbackWait {
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
                let _ = request.respond(success_response());
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

fn success_response() -> Response<std::io::Cursor<Vec<u8>>> {
    let body = "<html><body>Authentication complete. You can close this tab.</body></html>";
    html_response(body)
}

fn provider_error_response() -> Response<std::io::Cursor<Vec<u8>>> {
    let body = "<html><body>Authentication failed. You can close this tab.</body></html>";
    html_response(body)
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
            FlowError::NotATerminal.to_string(),
            "stdin/stderr is not a TTY; OAuth requires an interactive browser session"
        );
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
