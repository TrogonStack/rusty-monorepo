use super::*;
use crate::telemetry::testing::capture;

#[test]
fn the_callback_wait_records_how_it_ended() {
    let ((), trace) = capture(|| {
        CallbackWait::Timeout.record_on(&callback_wait_span("weather"));
    });

    let outcome = |key| {
        trace
            .attribute("wait oauth_callback", key)
            .map(|value| value.to_string())
    };
    assert_eq!(outcome(OAUTH_CALLBACK_OUTCOME).as_deref(), Some("timeout"));
    assert_eq!(outcome(ERROR_TYPE).as_deref(), Some("timeout"));
}

#[test]
fn a_received_callback_is_not_an_error() {
    let ((), trace) = capture(|| {
        CallbackWait::Success {
            code: "code".to_string(),
            state: "state".to_string(),
        }
        .record_on(&callback_wait_span("weather"));
    });

    assert_eq!(
        trace
            .attribute("wait oauth_callback", OAUTH_CALLBACK_OUTCOME)
            .map(|value| value.to_string())
            .as_deref(),
        Some("received")
    );
    assert!(trace.attribute("wait oauth_callback", ERROR_TYPE).is_none());
}

/// The Keychain backend accepts any server name, so one carrying markup
/// reaches this page. It has to arrive as text.
#[test]
fn a_server_name_is_escaped_into_the_page() {
    let body = read_body(success_response("<script>alert(1)</script>"));

    assert!(!body.contains("<script>"), "{body}");
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{body}");
}

/// This page is served the instant the callback arrives, before the code
/// is exchanged and before anything reaches the secrets backend. Claiming
/// otherwise would have the browser contradict the terminal whenever the
/// exchange or the write then fails.
#[test]
fn the_success_page_does_not_claim_a_credential_was_stored() {
    let body = read_body(success_response("linear"));

    assert!(!body.contains("stored"), "{body}");
    assert!(body.contains("finishing in your terminal"), "{body}");
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

/// A placeholder the template spells differently from [`page`] would ship
/// as literal text on a page nothing else renders in CI.
#[test]
fn every_placeholder_in_the_template_is_substituted() {
    let body = read_body(success_response("linear"));

    assert!(!body.contains("{{"), "unsubstituted placeholder in {body}");
}

/// Write both pages out and print where, so they can be opened in a real
/// browser. This is the only way to look at them: the flow they belong to
/// needs a terminal and a provider round trip.
///
/// Ignored because it asserts nothing.
///
/// ```sh
/// cargo test -p trg --lib flow::tests::preview -- --ignored --nocapture
/// ```
#[test]
#[ignore = "writes files to look at by hand"]
fn preview() {
    let dir = std::env::temp_dir().join("trg-callback-preview");
    std::fs::create_dir_all(&dir).expect("create");

    for (name, response) in [
        ("authorized.html", success_response("linear")),
        ("failed.html", provider_error_response()),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, read_body(response)).expect("write");
        println!("{}", path.display());
    }
}

/// The body a browser would render, taken from the bytes actually served
/// rather than from `page` directly, so the headers are on the wire too.
fn read_body(response: Response<std::io::Cursor<Vec<u8>>>) -> String {
    let mut out = Vec::new();
    response
        .raw_print(&mut out, (1, 1).into(), &[], false, None)
        .expect("render");
    let served = String::from_utf8(out).expect("utf-8");

    let (headers, body) = served.split_once("\r\n\r\n").expect("a header/body break");
    assert!(headers.contains("Content-Type: text/html"), "{headers}");
    body.to_string()
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
