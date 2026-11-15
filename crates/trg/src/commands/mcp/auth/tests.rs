use serde_json::json;

use rmcp::transport::auth::StoredCredentials;
use secrecy::SecretString;

use super::*;
use crate::oauth::store::CREDENTIALS_KEY;
use crate::secrets::fake::FakeBackend;
use crate::secrets::{openbao, Backend, FakeFailure, SecretKey, SecretMap};

fn stored() -> StoredCredentials {
    StoredCredentials::new("client".to_string(), None, vec!["scope".to_string()], Some(42))
}

fn openbao_backend() -> Backend {
    Backend::OpenBao(
        openbao::OpenBaoBackend::new(openbao::OpenBaoSettings {
            addr: "https://bao.example.com:8200".to_string(),
            mount: "secret".to_string(),
            path_prefix: "trg".to_string(),
            owner: "yordis".to_string(),
            machine_id: Some("laptop".to_string()),
            token: openbao::TokenSource::Var(crate::config::VarSource::Literal("t".to_string())),
            ca_cert_file: None,
            timeout: std::time::Duration::from_secs(5),
        })
        .expect("build"),
    )
}

async fn seed(backend: &Backend, path: &SecretPath, client_id: &str) {
    let json = serde_json::to_string(&StoredCredentials::new(
        client_id.to_string(),
        None,
        vec!["scope".to_string()],
        Some(42),
    ))
    .expect("encode");
    let mut map = SecretMap::new();
    map.insert(SecretKey::parse(CREDENTIALS_KEY).unwrap(), SecretString::from(json));
    backend.set(path, &map).await.expect("seed");
}

/// The text output says when a credential was read from the shared path.
/// Anything reading the JSON instead is making the same decision off the
/// same load, so leaving the field out would hand it the configured path
/// as if that were where the credential lives.
#[test]
fn json_status_names_the_shared_path_a_credential_was_actually_read_from() {
    let shared = SecretPath::parse("mcp/github").expect("parse");
    let stored = stored();

    let document = serde_json::to_value(StoredCredentialsView::from(&stored, None, Some(&shared))).expect("serialize");

    assert_eq!(document["read_from_shared_path"], "mcp/github");
}

#[test]
fn json_status_says_nothing_about_a_shared_path_when_the_primary_answered() {
    let stored = stored();

    let document = serde_json::to_value(StoredCredentialsView::from(&stored, None, None)).expect("serialize");

    assert!(
        document.get("read_from_shared_path").is_none(),
        "a primary hit must not imply a fallback happened"
    );
}

#[test]
fn cleared_text_names_the_shared_path_and_the_removal_command_when_one_is_still_reachable() {
    let shared = SecretPath::parse("mcp/github").expect("parse");

    let message = cleared_message(
        "github",
        Some(&shared),
        Some("bao kv metadata delete secret/trg/yordis/mcp/github"),
    );

    assert!(message.contains("mcp/github"), "must name the shared path: {message}");
    assert!(
        message.contains("bao kv metadata delete secret/trg/yordis/mcp/github"),
        "must name the removal command: {message}"
    );
    assert!(
        message.contains("keep authenticating"),
        "must say this machine stays signed in through it: {message}"
    );
}

/// A backend with no removal command to give (none today, but the call
/// site must not assume OpenBao is the only kind that can end up here)
/// still says the credential is reachable, just without a command to
/// paste.
#[test]
fn cleared_text_still_names_the_shared_path_when_the_backend_has_no_removal_command() {
    let shared = SecretPath::parse("mcp/github").expect("parse");

    let message = cleared_message("github", Some(&shared), None);

    assert!(message.contains("mcp/github"), "must name the shared path: {message}");
    assert!(
        message.contains("directly against the backend"),
        "must still point at a remedy: {message}"
    );
}

#[test]
fn cleared_text_reports_a_clean_logout_when_nothing_is_left_reachable() {
    let message = cleared_message("github", None, None);

    assert_eq!(message, "OAuth credentials cleared for `github`.");
}

#[test]
fn cleared_json_says_more_than_cleared_true_when_a_shared_path_is_still_reachable() {
    let shared = SecretPath::parse("mcp/github").expect("parse");

    let document = cleared_document(
        "github",
        Some(&shared),
        Some("bao kv metadata delete secret/trg/yordis/mcp/github"),
    );

    assert_eq!(document["cleared"], true);
    assert_eq!(document["shared_path"], "mcp/github");
    assert!(
        document["note"]
            .as_str()
            .unwrap()
            .contains("bao kv metadata delete secret/trg/yordis/mcp/github"),
        "the note must name the removal command: {document}"
    );
}

#[test]
fn cleared_json_reports_cleared_true_alone_when_nothing_is_left_reachable() {
    let document = cleared_document("github", None, None);

    assert_eq!(document, json!({ "server": "github", "cleared": true }));
}

/// The hole this covers: a `SecretPath` is backend-relative, so a note
/// naming only `mcp/github` prints a command that either fails to
/// address anything or, worse, addresses some other secret under a
/// mount named `mcp`. `cleared` has to ask the backend for the fully
/// qualified command, mount and storage prefix and all, rather than
/// printing the relative path itself.
#[test]
fn cleared_names_the_fully_qualified_removal_command_not_just_the_relative_path() {
    let shared = SecretPath::parse("mcp/github").expect("parse");
    let removal_command = openbao_backend()
        .removal_command(&shared)
        .expect("openbao offers a removal command");

    assert_eq!(removal_command, "bao kv metadata delete secret/trg/yordis/mcp/github");
    let document = cleared_document("github", Some(&shared), Some(&removal_command));

    assert_eq!(
        document["note"].as_str().unwrap(),
        "this machine will keep authenticating from the shared credential at `mcp/github`; remove it \
             with `bao kv metadata delete secret/trg/yordis/mcp/github` to sign out every machine using it"
    );
}

/// A machine with no fallback (or an already-empty one) logs out clean:
/// `logout` reports nothing still reachable.
#[tokio::test]
async fn logout_with_no_credential_left_reports_nothing_reachable() {
    let backend = Backend::Fake(FakeBackend::new());
    let cred_path = SecretPath::parse("mcp/laptop/github").expect("parse");
    seed(&backend, &cred_path, "client").await;
    let ctx = McpContext::credentials_only("github".to_string(), backend, cred_path, None);

    let still_reachable = logout(&ctx).await.expect("logout");

    assert!(still_reachable.is_none());
}

/// The hole this covers: `clear` only ever deletes the machine-scoped
/// path, so a machine that still has a shared fallback credential is not
/// actually signed out. `logout` must say so rather than reporting a
/// clean logout.
#[tokio::test]
async fn logout_with_a_populated_fallback_reports_the_shared_path_still_reachable() {
    let backend = Backend::Fake(FakeBackend::new());
    let cred_path = SecretPath::parse("mcp/laptop/github").expect("parse");
    let shared = SecretPath::parse("mcp/github").expect("parse");
    seed(&backend, &cred_path, "machine-cred").await;
    seed(&backend, &shared, "shared-cred").await;
    let ctx = McpContext::credentials_only(
        "github".to_string(),
        backend.clone(),
        cred_path.clone(),
        Some(shared.clone()),
    );

    let still_reachable = logout(&ctx).await.expect("logout");

    assert_eq!(still_reachable, Some(shared));
    assert!(
        backend.get(&cred_path).await.expect("get").is_none(),
        "the machine-scoped credential must still be deleted"
    );
}

/// The hole this covers: `load` turns a fallback read error into a clean
/// miss, which is right for a courtesy read but wrong for `logout`, which
/// has to answer whether this machine is still signed in. A storage error
/// while checking must surface rather than read as "nothing there".
#[tokio::test]
async fn logout_propagates_a_fallback_read_error_instead_of_reporting_a_clean_logout() {
    let backend = Backend::Fake(FakeBackend::new());
    let cred_path = SecretPath::parse("mcp/laptop/github").expect("parse");
    let shared = SecretPath::parse("mcp/github").expect("parse");
    seed(&backend, &cred_path, "machine-cred").await;
    let Backend::Fake(fake) = &backend else {
        unreachable!("built as fake above");
    };
    fake.set_get_failure_at(shared.clone(), FakeFailure::Transport);
    let ctx = McpContext::credentials_only("github".to_string(), backend, cred_path, Some(shared));

    let err = logout(&ctx)
        .await
        .expect_err("a fallback read error must not read as a clean logout");

    assert!(matches!(err, AuthError::Store(_)), "{err}");
}
