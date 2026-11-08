use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use opentelemetry::trace::Status;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::secrets::openbao::OpenBaoSettings;

/// Answers `sys/health` one way and everything else another, recording what
/// it was asked so a test can assert on what was never sent.
struct Stub {
    addr: String,
    seen: Arc<Mutex<Vec<String>>>,
    _server: Arc<tiny_http::Server>,
}

impl Stub {
    fn start(health: (u16, &'static str), other: (u16, &'static str)) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
        let addr = format!(
            "http://{}",
            server.server_addr().to_ip().expect("a loopback tcp address")
        );
        let seen = Arc::new(Mutex::new(Vec::new()));

        let worker = Arc::clone(&server);
        let recorder = Arc::clone(&seen);
        std::thread::spawn(move || {
            while let Ok(request) = worker.recv() {
                let url = request.url().to_string();
                recorder.lock().expect("stub lock").push(url.clone());

                let (status, body) = if url.starts_with("/v1/sys/health") {
                    health
                } else {
                    other
                };
                let response = tiny_http::Response::from_string(body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                            .expect("static header"),
                    );
                let _ = request.respond(response);
            }
        });

        Self {
            addr,
            seen,
            _server: server,
        }
    }

    fn backend(&self, token: TokenSource) -> OpenBaoBackend {
        OpenBaoBackend::new(OpenBaoSettings {
            addr: self.addr.clone(),
            mount: "secret".to_string(),
            path_prefix: "trg".to_string(),
            owner: "yordis".to_string(),
            machine_id: None,
            token,
            ca_cert_file: None,
            timeout: Duration::from_secs(2),
        })
        .expect("build")
    }

    fn asked(&self) -> Vec<String> {
        self.seen.lock().expect("stub lock").clone()
    }
}

const SERVING: &str = r#"{"initialized":true,"sealed":false,"standby":false,"version":"2.6.2"}"#;
const SEALED: &str = r#"{"initialized":true,"sealed":true,"standby":false,"version":"2.6.2"}"#;
const TWO_KEYS: &str = r#"{"data":{"keys":["mcp/","other"]}}"#;
const NO_MOUNT: &str = r#"{"errors":["no handler for route \"nope/metadata/trg/\""]}"#;
const DENIED: &str = r#"{"errors":["permission denied"]}"#;

fn literal() -> TokenSource {
    TokenSource::Var(crate::config::VarSource::Literal("s.not-a-real-token".to_string()))
}

fn outcome<'a>(report: &'a Report, name: &str) -> &'a Outcome {
    &report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no `{name}` check in {:?}", report.checks))
        .outcome
}

#[tokio::test]
async fn a_reachable_instance_the_token_can_list_passes_everything() {
    let stub = Stub::start((200, SERVING), (200, TWO_KEYS));
    let report = openbao("live", &stub.backend(literal())).await;

    for name in ["token", "instance", "mount", "subtree"] {
        assert!(
            matches!(outcome(&report, name), Outcome::Passed { .. }),
            "{name}: {:?}",
            outcome(&report, name)
        );
    }
    assert_eq!(report.exit_code(), ExitCode::Success);
    assert!(matches!(outcome(&report, "subtree"), Outcome::Passed { detail } if detail == "listable, 2 entries"));
}

/// `sys/health` reports a sealed instance with `503`, which is an answer
/// rather than a failure. Reading it as one would report a sealed instance
/// as unreachable and send the operator after the network instead.
#[tokio::test]
async fn a_sealed_instance_is_reported_as_sealed_rather_than_unreachable() {
    let stub = Stub::start((503, SEALED), (200, TWO_KEYS));
    let report = openbao("live", &stub.backend(literal())).await;

    let Outcome::Failed { detail, remedy } = outcome(&report, "instance") else {
        panic!("expected a failure, got {:?}", outcome(&report, "instance"));
    };
    assert_eq!(detail, "sealed (OpenBao 2.6.2)");
    assert!(remedy.contains("unseal"), "{remedy}");

    for name in ["mount", "subtree"] {
        assert!(matches!(outcome(&report, name), Outcome::Skipped { .. }), "{name}");
    }
    assert_eq!(report.exit_code(), ExitCode::Failed);
}

/// An `addr` pointed at something that answers JSON but is not OpenBao
/// must not read back as an OpenBao that was never initialized, because the
/// remedy for that is to initialize whatever is actually listening there.
#[tokio::test]
async fn a_body_that_is_not_a_health_report_is_not_read_as_an_uninitialized_instance() {
    let stub = Stub::start((200, r#"{"errors":["missing client token"]}"#), (200, TWO_KEYS));
    let report = openbao("live", &stub.backend(literal())).await;

    let Outcome::Failed { detail, remedy } = outcome(&report, "instance") else {
        panic!("expected a failure, got {:?}", outcome(&report, "instance"));
    };
    assert!(!detail.contains("uninitialized"), "{detail}");
    assert!(!remedy.contains("init"), "{remedy}");
    assert!(remedy.contains("`addr`"), "{remedy}");
}

/// Only a refusal says the request reached the subtree. Anything else
/// stopped at the mount, and blaming the policy for it sends an operator to
/// rewrite a policy that was never consulted.
#[tokio::test]
async fn a_mount_that_answers_nonsense_is_not_blamed_on_the_policy() {
    let stub = Stub::start((200, SERVING), (200, "not json at all"));
    let report = openbao("live", &stub.backend(literal())).await;

    let Outcome::Failed { remedy, .. } = outcome(&report, "mount") else {
        panic!("expected a failure, got {:?}", outcome(&report, "mount"));
    };
    assert!(remedy.contains("`addr`"), "{remedy}");
    assert!(matches!(outcome(&report, "subtree"), Outcome::Skipped { .. }));
}

/// A timeout and a missing mount arrive as the same error, so the remedy
/// has to hold for both rather than assert the mount name is wrong.
#[test]
fn the_remedy_for_a_mount_that_did_not_answer_covers_more_than_a_wrong_name() {
    let remedy = mount_remedy(&SecretsError::Unavailable("timed out".to_string()), "secret");
    assert!(remedy.contains("bao secrets list"), "{remedy}");
    assert!(remedy.contains("reachable"), "{remedy}");
}

#[tokio::test]
async fn a_missing_mount_is_reported_against_the_mount() {
    let stub = Stub::start((200, SERVING), (404, NO_MOUNT));
    let report = openbao("live", &stub.backend(literal())).await;

    assert!(matches!(outcome(&report, "mount"), Outcome::Failed { .. }));
    assert!(matches!(outcome(&report, "subtree"), Outcome::Skipped { .. }));
}

/// A policy scoped to one subtree denies before the mount is looked up, so
/// a refusal is not evidence that the mount is there. Reporting it as
/// evidence would send someone to check a mount that was never in question.
#[tokio::test]
async fn a_refusal_does_not_claim_the_mount_was_reached() {
    let stub = Stub::start((200, SERVING), (403, DENIED));
    let report = openbao("live", &stub.backend(literal())).await;

    assert!(matches!(outcome(&report, "mount"), Outcome::Skipped { .. }));
    assert!(matches!(outcome(&report, "subtree"), Outcome::Failed { .. }));
}

/// Probing with a token that could not be read would report the mount as
/// answering something it was never asked.
#[tokio::test]
async fn an_unreadable_token_stops_before_anything_is_authenticated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = Stub::start((200, SERVING), (200, TWO_KEYS));
    let report = openbao(
        "live",
        &stub.backend(TokenSource::File(dir.path().join("was-never-written"))),
    )
    .await;

    assert!(matches!(outcome(&report, "token"), Outcome::Failed { .. }));
    for name in ["mount", "subtree"] {
        assert!(matches!(outcome(&report, name), Outcome::Skipped { .. }), "{name}");
    }
    assert_eq!(
        stub.asked(),
        vec!["/v1/sys/health".to_string()],
        "nothing but the unauthenticated probe should have been sent"
    );
}

/// The report names where the token comes from so an operator can find it,
/// and that is the closest it may ever get to the token itself.
#[tokio::test]
async fn no_rendering_of_the_report_carries_the_token() {
    let stub = Stub::start((200, SERVING), (200, TWO_KEYS));
    let report = openbao("live", &stub.backend(literal())).await;

    let text = report.to_text();
    let json = serde_json::to_string(&report).expect("serializes");

    for rendered in [&text, &json] {
        assert!(!rendered.contains("s.not-a-real-token"), "{rendered}");
    }
    assert!(text.contains("a literal in the config file"), "{text}");
}

#[test]
fn a_report_is_unhealthy_when_any_check_failed_and_not_when_one_was_skipped() {
    let skipped = Report {
        backend: "live".to_string(),
        kind: "openbao",
        target: "somewhere".to_string(),
        checks: vec![Check::new("subtree", Outcome::skipped("nothing to enumerate"))],
    };
    assert!(skipped.is_healthy());
    assert_eq!(skipped.exit_code(), ExitCode::Success);

    let failed = Report {
        checks: vec![Check::new("mount", Outcome::failed("gone", "put it back"))],
        ..skipped
    };
    assert!(!failed.is_healthy());
    assert_eq!(failed.exit_code(), ExitCode::Failed);
}

/// Nothing declared is not nothing used, and the platform check is the one
/// that fails on a machine where the default cannot work at all.
#[tokio::test]
async fn declaring_no_backend_still_diagnoses_the_one_that_will_be_used() {
    let empty = Registry::new(crate::secrets::SecretsSection::default());
    let diagnosis = diagnose_all(&empty, None).await.expect("nothing to resolve");

    assert_eq!(diagnosis.backends.len(), 1);
    assert_eq!(diagnosis.backends[0].backend, IMPLICIT);
    assert_eq!(diagnosis.backends[0].kind, "keychain");
    assert_eq!(diagnosis.is_healthy(), cfg!(target_os = "macos"));
}

/// The whole point of checking every backend at once is lost if the first
/// broken one hides the rest, and a backend too broken to build is the most
/// likely one to be first.
#[tokio::test]
async fn one_backend_that_will_not_build_does_not_hide_the_others() {
    let toml = r#"
            [secrets.backends.aaa]
            kind = "openbao"
            addr = { env = "A_VAR_THAT_IS_NOT_SET_ANYWHERE" }
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token = "irrelevant"

            [secrets.backends.zzz]
            kind = "keychain"
            service = "whatever"
        "#;
    let section: crate::secrets::SecretsSection = toml::from_str::<toml::Value>(toml)
        .expect("valid toml")
        .get("secrets")
        .expect("a secrets table")
        .clone()
        .try_into()
        .expect("a secrets section");

    let diagnosis = diagnose_all(&Registry::new(section), None)
        .await
        .expect("both declared");

    assert_eq!(diagnosis.backends.len(), 2, "{:?}", diagnosis.backends);
    assert_eq!(diagnosis.backends[0].backend, "aaa");
    assert_eq!(
        diagnosis.backends[0].kind, "openbao",
        "the declared kind survives a failed build"
    );
    assert!(matches!(
        diagnosis.backends[0].checks[0].outcome,
        Outcome::Failed { .. }
    ));
    assert_eq!(diagnosis.backends[1].backend, "zzz");
    assert_eq!(diagnosis.backends[1].kind, "keychain");
}

#[tokio::test]
async fn naming_a_backend_that_does_not_exist_is_still_an_error_rather_than_a_report() {
    let empty = Registry::new(crate::secrets::SecretsSection::default());
    let err = diagnose_all(&empty, Some("nosuch"))
        .await
        .expect_err("nothing declared");
    assert!(err.to_string().contains("nosuch"), "{err}");
}

fn report(backend: &str, checks: Vec<Check>) -> Report {
    Report {
        backend: backend.to_string(),
        kind: "openbao",
        target: "somewhere".to_string(),
        checks,
    }
}

#[test]
fn one_backend_reads_exactly_as_it_did_before_there_were_several() {
    let one = Diagnosis {
        backends: vec![report("work", vec![Check::new("token", Outcome::passed("a literal"))])],
    };

    assert_eq!(one.to_text(), one.backends[0].to_text());
    assert!(!one.to_text().contains("backends checked"));
}

#[test]
fn several_backends_are_counted_so_the_failing_one_is_not_scrolled_past() {
    let several = Diagnosis {
        backends: vec![
            report("work", vec![Check::new("token", Outcome::passed("a literal"))]),
            report("ci", vec![Check::new("mount", Outcome::failed("gone", "put it back"))]),
        ],
    };

    assert!(
        several.to_text().contains("2 backends checked, 1 failed"),
        "{}",
        several.to_text()
    );
    assert!(!several.is_healthy());
    assert_eq!(several.exit_code(), ExitCode::Failed);
}

#[test]
fn a_diagnosis_is_healthy_only_when_every_backend_is() {
    let healthy = Diagnosis {
        backends: vec![
            report("work", vec![Check::new("token", Outcome::passed("a literal"))]),
            report(
                "ci",
                vec![Check::new("subtree", Outcome::skipped("nothing to enumerate"))],
            ),
        ],
    };

    assert!(healthy.is_healthy());
    assert_eq!(healthy.exit_code(), ExitCode::Success);
}

#[test]
fn a_failure_is_rendered_with_what_to_do_about_it() {
    let report = Report {
        backend: "live".to_string(),
        kind: "openbao",
        target: "somewhere".to_string(),
        checks: vec![Check::new("mount", Outcome::failed("gone", "put it back"))],
    };

    let text = report.to_text();
    assert!(text.contains("FAILED"), "{text}");
    assert!(text.contains("gone"), "{text}");
    assert!(text.contains("put it back"), "{text}");
}

#[test]
fn a_diagnosis_is_a_span_over_the_requests_it_makes() {
    let stub = Stub::start((200, SERVING), (200, TWO_KEYS));
    let backend = Backend::OpenBao(stub.backend(literal()));
    let (report, trace) = crate::telemetry::testing::capture(|| current_thread(diagnose("live", &backend)));
    assert!(report.is_healthy());

    let attribute = |key: &str| trace.attribute("diagnose backend", key).map(|v| v.to_string());
    assert_eq!(attribute("trg.secrets.backend.name").as_deref(), Some("live"));
    assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("openbao"));
    assert_eq!(attribute("trg.doctor.healthy").as_deref(), Some("true"));
    trace.assert_child_of("GET /v1/sys/health", "diagnose backend");
    trace.assert_child_of("HTTP /v1/{mount}/metadata/{path}", "diagnose backend");

    assert_never_recorded(&trace, &["s.not-a-real-token", "yordis"]);
}

/// A sealed instance answers `sys/health` with `503`, which is the state
/// being reported rather than a failed request.
#[test]
fn a_sealed_instance_is_unhealthy_without_failing_the_health_request() {
    let stub = Stub::start((503, SEALED), (200, TWO_KEYS));
    let backend = Backend::OpenBao(stub.backend(literal()));
    let (report, trace) = crate::telemetry::testing::capture(|| current_thread(diagnose("live", &backend)));
    assert!(!report.is_healthy());

    assert_eq!(
        trace
            .attribute("diagnose backend", "trg.doctor.healthy")
            .map(|v| v.to_string())
            .as_deref(),
        Some("false")
    );
    let health = trace.span("GET /v1/sys/health").expect("the health request");
    assert!(!matches!(health.status, Status::Error { .. }), "{:?}", health.status);
    assert_eq!(
        trace
            .attribute("GET /v1/sys/health", "http.response.status_code")
            .map(|v| v.to_string())
            .as_deref(),
        Some("503")
    );
}
