use super::*;
use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use crate::secrets::SecretKey;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Status;
use secrecy::{ExposeSecret, SecretString};
use std::os::unix::fs::PermissionsExt as _;

/// A stand-in for `/usr/bin/security` that records its argv and answers
/// with a fixed exit code, stdout and stderr.
///
/// The backend under test is the production one driving a real child
/// process, so what these tests pin down is the real argv and the real
/// stderr tokens, not a re-description of them.
struct StubSecurity {
    dir: tempfile::TempDir,
}

fn sh_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

impl StubSecurity {
    fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("security");
        let argv = dir.path().join("argv");
        let traceparent = dir.path().join("traceparent");
        let script = format!(
                "#!/bin/sh\nprintf '%s' \"${{TRACEPARENT:-}}\" > {traceparent}\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\nprintf '%s' {stdout}\nprintf '%s' {stderr} >&2\nexit {exit}\n",
                traceparent = sh_quote(&traceparent.display().to_string()),
                argv = sh_quote(&argv.display().to_string()),
                stdout = sh_quote(stdout),
                stderr = sh_quote(stderr),
            );
        std::fs::write(&bin, script).expect("write stub");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        Self { dir }
    }

    fn ok(stdout: &str) -> Self {
        Self::answering(0, stdout, "")
    }

    fn backend(&self) -> KeychainBackend {
        KeychainBackend::with_bin("svc", self.dir.path().join("security"))
    }

    fn traceparent(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("traceparent")).unwrap_or_default()
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn test_path(ns: u32) -> SecretPath {
    SecretPath::parse(&format!("trg-test-{}-{ns}", std::process::id())).expect("parse")
}

fn map_of(pairs: &[(&str, &str)]) -> SecretMap {
    let mut map = SecretMap::new();
    for (k, v) in pairs {
        map.insert(SecretKey::parse(k).expect("key"), SecretString::from(*v));
    }
    map
}

const NOT_FOUND: &str = "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.";

#[tokio::test]
async fn get_asks_for_the_password_of_this_service_and_account() {
    let stub = StubSecurity::ok("{\"credentials\":\"{}\"}\n");
    let map = stub
        .backend()
        .get(&SecretPath::parse("github").unwrap())
        .await
        .expect("get")
        .expect("some");

    assert_eq!(
        map.get(&SecretKey::parse("credentials").unwrap())
            .map(|v| v.expose_secret()),
        Some("{}")
    );
    assert_eq!(
        stub.argv(),
        ["find-generic-password", "-s", "svc", "-a", "github", "-w"]
    );
}

#[tokio::test]
async fn get_of_an_absent_item_is_a_miss_not_an_error() {
    let stub = StubSecurity::answering(44, "", NOT_FOUND);
    assert!(stub
        .backend()
        .get(&SecretPath::parse("github").unwrap())
        .await
        .expect("get")
        .is_none());
}

#[tokio::test]
async fn a_blocked_prompt_is_reported_as_a_permission_problem() {
    let stub = StubSecurity::answering(
        36,
        "",
        "security: SecKeychainSearchCopyNext: User interaction is not allowed.",
    );
    let err = stub
        .backend()
        .get(&SecretPath::parse("github").unwrap())
        .await
        .expect_err("should fail");

    assert!(matches!(err, SecretsError::PermissionDenied(_)), "{err:?}");
}

#[tokio::test]
async fn an_unrecognised_failure_stays_a_transport_error() {
    let stub = StubSecurity::answering(1, "", "security: the keychain is locked");
    let err = stub
        .backend()
        .get(&SecretPath::parse("github").unwrap())
        .await
        .expect_err("should fail");

    assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn a_payload_that_is_not_a_secret_map_is_malformed() {
    let stub = StubSecurity::ok("not json\n");
    let path = SecretPath::parse("github").unwrap();
    let err = stub.backend().get(&path).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
}

#[tokio::test]
async fn set_creates_the_item_with_an_empty_trusted_application_list() {
    let stub = StubSecurity::ok("");
    stub.backend()
        .set(&SecretPath::parse("github").unwrap(), &map_of(&[("k", "v")]))
        .await
        .expect("set");

    assert_eq!(
        stub.argv(),
        [
            "add-generic-password",
            "-U",
            "-A",
            "-s",
            "svc",
            "-a",
            "github",
            "-w",
            "{\"k\":\"v\"}"
        ],
        "`-U -A` is what keeps a rebuilt binary from re-prompting"
    );
}

#[tokio::test]
async fn delete_of_an_absent_item_is_idempotent() {
    let stub = StubSecurity::answering(44, "", NOT_FOUND);
    stub.backend()
        .delete(&SecretPath::parse("github").unwrap())
        .await
        .expect("delete");

    assert_eq!(stub.argv(), ["delete-generic-password", "-s", "svc", "-a", "github"]);
}

#[tokio::test]
async fn a_failing_delete_that_is_not_a_miss_is_reported() {
    let stub = StubSecurity::answering(1, "", "security: the keychain is locked");
    let err = stub
        .backend()
        .delete(&SecretPath::parse("github").unwrap())
        .await
        .expect_err("should fail");

    assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn an_unspawnable_security_binary_is_a_transport_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = KeychainBackend::with_bin("svc", dir.path().join("absent"));
    let err = backend
        .get(&SecretPath::parse("github").unwrap())
        .await
        .expect_err("should fail");

    assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn list_is_unsupported() {
    let stub = StubSecurity::ok("");
    assert!(matches!(
        stub.backend().list(None).await,
        Err(SecretsError::Unsupported { op: "list", .. })
    ));
}

/// Round-trips against the real login keychain.
///
/// Ignored by default: the legacy file-based keychain serialises poorly
/// under the parallelism of the full test binary, and a lost write there
/// says nothing about this crate. Run with
/// `cargo test -p trg --lib keychain::tests::live -- --ignored --test-threads=1`.
#[tokio::test]
#[ignore = "writes to the developer's real login keychain"]
async fn live_set_get_delete_roundtrip() {
    let path = test_path(1);
    let backend = KeychainBackend::with_default_service();

    backend
        .set(&path, &map_of(&[("credentials", "{\"a\":1}")]))
        .await
        .expect("set");

    let loaded = backend.get(&path).await.expect("get").expect("some");
    assert_eq!(
        loaded
            .get(&SecretKey::parse("credentials").unwrap())
            .map(|v| v.expose_secret()),
        Some("{\"a\":1}")
    );

    backend
        .set(&path, &map_of(&[("credentials", "{\"a\":2}")]))
        .await
        .expect("overwrite");
    let loaded = backend.get(&path).await.expect("get").expect("some");
    assert_eq!(loaded.len(), 1);

    backend.delete(&path).await.expect("delete");
    assert!(backend.get(&path).await.expect("get").is_none());
    backend.delete(&path).await.expect("delete again");
}

#[test]
fn each_call_is_a_cli_span_that_the_child_continues() {
    let stub = StubSecurity::ok("{\"credentials\":\"{}\"}\n");
    let backend = stub.backend();
    let (read, trace) =
        crate::telemetry::testing::capture(|| current_thread(backend.get(&SecretPath::parse("github").unwrap())));
    assert!(read.expect("get").is_some());

    let span = trace.span("security").expect("a span per call");
    assert_eq!(span.span_kind, SpanKind::Client);
    let attribute = |key: &str| trace.attribute("security", key).map(|v| v.to_string());
    assert_eq!(attribute("process.executable.name").as_deref(), Some("security"));
    assert_eq!(attribute("process.exit.code").as_deref(), Some("0"));
    assert!(matches!(
        trace.attribute("security", "process.pid"),
        Some(opentelemetry::Value::I64(_))
    ));
    assert_eq!(
        attribute("trg.secrets.operation").as_deref(),
        Some("find-generic-password")
    );
    assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("keychain"));

    let traceparent = stub.traceparent();
    assert_eq!(
        traceparent,
        format!("00-{}-{}-01", span.span_context.trace_id(), span.span_context.span_id()),
        "the child continues the call's span"
    );
    assert_never_recorded(&trace, &["github", "svc"]);
}

#[test]
fn a_write_never_records_the_payload_it_hands_security() {
    const SECRET: &str = "keychain-span-secret-5b7c";
    let stub = StubSecurity::ok("");
    let backend = stub.backend();
    let (written, trace) = crate::telemetry::testing::capture(|| {
        current_thread(backend.set(&SecretPath::parse("github").unwrap(), &map_of(&[("token", SECRET)])))
    });
    written.expect("set");

    assert_eq!(
        trace
            .attribute("security", "trg.secrets.operation")
            .map(|v| v.to_string())
            .as_deref(),
        Some("add-generic-password")
    );
    assert_never_recorded(&trace, &[SECRET, "github", "token"]);
}

#[test]
fn a_failing_call_is_typed_by_its_exit() {
    let stub = StubSecurity::answering(44, "", NOT_FOUND);
    let backend = stub.backend();
    let (read, trace) =
        crate::telemetry::testing::capture(|| current_thread(backend.get(&SecretPath::parse("github").unwrap())));
    assert!(read.expect("a miss is not an error").is_none());

    let span = trace.span("security").expect("a span per call");
    assert!(matches!(span.status, Status::Error { .. }));
    let attribute = |key: &str| trace.attribute("security", key).map(|v| v.to_string());
    assert_eq!(attribute("process.exit.code").as_deref(), Some("44"));
    assert_eq!(attribute("error.type").as_deref(), Some("nonzero_exit"));
}
