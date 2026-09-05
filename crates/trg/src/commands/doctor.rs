//! The checks behind `trg doctor`.
//!
//! `doctor` sits at the top level rather than under the subsystem it checks,
//! because it is what an operator reaches for before knowing which subsystem
//! is at fault. Filing it under `secrets` would mean having to know the answer
//! to find the command.
//!
//! Every probe here is a read. A doctor that wrote to prove writing works
//! would need a path it is allowed to clobber, and picking one on the
//! operator's behalf is exactly the kind of decision a diagnostic should not
//! be making.
//!
//! The authenticated probe is the same `list` the backend already performs in
//! normal operation, deliberately. Asking `sys/mounts` would report the mount
//! more directly and would also fail for a token scoped to one subtree, which
//! is the configuration this backend is meant to encourage: a doctor that
//! demands more privilege than the tool it diagnoses reports healthy setups as
//! broken.

use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::secrets::openbao::{Health, TokenSource};
use crate::secrets::{Backend, KeychainBackend, OpenBaoBackend, Registry, SecretsError};

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Check only this backend, as named under `[secrets.backends.<name>]`.
    /// Every declared backend is checked when this is omitted.
    #[arg(long)]
    pub backend: Option<String>,

    /// Output format. Neither format emits a secret value.
    #[arg(long, value_enum, default_value_t = DoctorFormat::Text)]
    pub format: DoctorFormat,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DoctorFormat {
    Text,
    Json,
}

/// What one check established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// It answered, and the answer was yes.
    Passed { detail: String },
    /// It answered, and the answer was no.
    Failed { detail: String, remedy: String },
    /// An earlier failure left this one unanswerable, so it was not attempted.
    Skipped { because: String },
}

impl Outcome {
    fn passed(detail: impl Into<String>) -> Self {
        Self::Passed { detail: detail.into() }
    }

    fn failed(detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
            remedy: remedy.into(),
        }
    }

    fn skipped(because: impl Into<String>) -> Self {
        Self::Skipped {
            because: because.into(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Passed { .. } => "ok",
            Self::Failed { .. } => "FAILED",
            Self::Skipped { .. } => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Check {
    fn new(name: &'static str, outcome: Outcome) -> Self {
        Self { name, outcome }
    }
}

/// Everything one `doctor` run found.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub backend: String,
    pub kind: &'static str,
    /// Where this backend puts things, in one line.
    pub target: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| matches!(c.outcome, Outcome::Failed { .. }))
    }

    pub fn exit_code(&self) -> i32 {
        i32::from(!self.is_healthy())
    }

    /// Aligned so the outcomes read down a column, since the whole point is to
    /// find the one line that is not `ok`.
    pub fn to_text(&self) -> String {
        let mut out = format!("backend  {} ({})\ntarget   {}\n", self.backend, self.kind, self.target);

        for check in &self.checks {
            out.push_str(&format!("\n  {:<8} {:<14} ", check.outcome.label(), check.name));
            match &check.outcome {
                Outcome::Passed { detail } => out.push_str(detail),
                Outcome::Skipped { because } => out.push_str(because),
                Outcome::Failed { detail, remedy } => {
                    out.push_str(detail);
                    out.push_str(&format!("\n  {:<8} {:<14} {remedy}", "", ""));
                }
            }
        }

        out.push('\n');
        out
    }
}

pub async fn diagnose(name: &str, backend: &Backend) -> Report {
    match backend {
        Backend::OpenBao(bao) => openbao(name, bao).await,
        Backend::Keychain(kc) => keychain(name, kc),
        #[cfg(test)]
        Backend::Fake(_) => Report {
            backend: name.to_string(),
            kind: backend.kind(),
            target: backend.describe(),
            checks: Vec::new(),
        },
    }
}

async fn openbao(name: &str, bao: &OpenBaoBackend) -> Report {
    let subtree = bao.storage_prefix();
    let target = format!(
        "{} (mount `{}`, subtree `{}`)",
        bao.addr(),
        bao.mount(),
        if subtree.is_empty() { "/" } else { &subtree }
    );

    let token = token_check(bao);
    let instance = instance_check(&bao.health().await);

    // Both of these gate the authenticated probe, and running it anyway would
    // report the mount as answering when nothing was ever sent to it.
    let blocked = if matches!(token.outcome, Outcome::Failed { .. }) {
        Some("the token could not be read")
    } else if matches!(instance.outcome, Outcome::Failed { .. }) {
        Some("the instance is not serving")
    } else {
        None
    };

    let mut checks = vec![token, instance];
    match blocked {
        Some(because) => {
            checks.push(Check::new("mount", Outcome::skipped(because)));
            checks.push(Check::new("subtree", Outcome::skipped(because)));
        }
        None => checks.extend(subtree_checks(bao).await),
    }

    Report {
        backend: name.to_string(),
        kind: "openbao",
        target,
        checks,
    }
}

/// Where the token comes from, and whether that source produces one.
///
/// Reads the token and throws it away: the source can be reported as working
/// without the value ever reaching the report.
fn token_check(bao: &OpenBaoBackend) -> Check {
    let source = match bao.token_source() {
        TokenSource::File(path) => format!("file `{}`", path.display()),
        TokenSource::Var(crate::config::VarSource::Env { env, .. }) => format!("env `{env}`"),
        TokenSource::Var(crate::config::VarSource::Literal(_)) => "a literal in the config file".to_string(),
    };

    match bao.token_is_readable() {
        Ok(()) => Check::new("token", Outcome::passed(source)),
        Err(e) => Check::new(
            "token",
            Outcome::failed(
                format!("{source}: {e}"),
                match bao.token_source() {
                    TokenSource::File(_) => "run `bao login` to write one",
                    TokenSource::Var(_) => "set the variable the config names",
                },
            ),
        ),
    }
}

fn instance_check(health: &Result<Health, SecretsError>) -> Check {
    match health {
        Ok(h) if h.is_serving() => Check::new("instance", Outcome::passed(h.summary())),
        Ok(h) if !h.initialized => Check::new(
            "instance",
            Outcome::failed(h.summary(), "run `bao operator init` against this instance"),
        ),
        Ok(h) => Check::new(
            "instance",
            Outcome::failed(h.summary(), "run `bao operator unseal` against this instance"),
        ),
        Err(e) => Check::new(
            "instance",
            Outcome::failed(
                e.to_string(),
                "check that `addr` names the instance and that it is reachable from here",
            ),
        ),
    }
}

/// One `list` of the configured subtree, read as two answers.
///
/// Reaching the mount and being allowed into the subtree fail differently, and
/// the backend already separates them, so the probe that normal operation
/// makes is enough to report both.
async fn subtree_checks(bao: &OpenBaoBackend) -> Vec<Check> {
    let mount = bao.mount();
    match bao.list(None).await {
        Ok(keys) => vec![
            Check::new("mount", Outcome::passed(format!("`{mount}` answers as KV v2"))),
            Check::new(
                "subtree",
                Outcome::passed(match keys.len() {
                    0 => "listable, nothing stored yet".to_string(),
                    1 => "listable, 1 entry".to_string(),
                    n => format!("listable, {n} entries"),
                }),
            ),
        ],
        // A policy scoped to one subtree denies before the mount is looked up,
        // so a refusal here says nothing about whether the mount is there.
        Err(e @ (SecretsError::Unauthorized { .. } | SecretsError::PermissionDenied(_))) => vec![
            Check::new(
                "mount",
                Outcome::skipped("the probe was refused before the mount was reached"),
            ),
            Check::new(
                "subtree",
                Outcome::failed(
                    e.to_string(),
                    "check that the token's policy covers this subtree, or run `bao login` again",
                ),
            ),
        ],
        // Everything else stopped at the mount rather than inside the subtree,
        // so the subtree was never in question and has nothing to report.
        Err(e) => {
            let remedy = mount_remedy(&e, mount);
            vec![
                Check::new("mount", Outcome::failed(e.to_string(), remedy)),
                Check::new("subtree", Outcome::skipped("the mount did not answer")),
            ]
        }
    }
}

/// `Unavailable` covers a missing mount, a sealed instance, and a request that
/// never arrived, so a remedy naming only the first would send an operator to
/// check a mount name that a timeout says nothing about.
fn mount_remedy(e: &SecretsError, mount: &str) -> String {
    match e {
        SecretsError::Unavailable(_) => format!(
            "check `mount` against `bao secrets list`, which lists what this instance actually \
             serves, and that it stayed reachable and unsealed; `{mount}` did not answer"
        ),
        _ => "check that `addr` names OpenBao itself rather than something standing in front of it".to_string(),
    }
}

/// The keychain is local, so there is nothing to reach and nothing to be
/// refused by. What can be established is that this build can talk to it.
fn keychain(name: &str, kc: &KeychainBackend) -> Report {
    let platform = if cfg!(target_os = "macos") {
        Outcome::passed("running on macOS")
    } else {
        Outcome::failed(
            "the keychain backend is available only on macOS",
            "declare an `openbao` backend instead on this platform",
        )
    };

    Report {
        backend: name.to_string(),
        kind: "keychain",
        target: format!("the macOS Keychain (service `{}`)", kc.service()),
        checks: vec![
            Check::new("platform", platform),
            Check::new(
                "subtree",
                Outcome::skipped("the keychain does not support listing, so there is nothing to enumerate"),
            ),
        ],
    }
}

/// Every subject one `doctor` run looked at.
///
/// A config declaring several backends is one an operator is midway through
/// setting up, which is exactly when checking them one at a time is the least
/// useful thing to be asked to do.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnosis {
    pub backends: Vec<Report>,
}

impl Diagnosis {
    pub fn is_healthy(&self) -> bool {
        self.backends.iter().all(Report::is_healthy)
    }

    pub fn exit_code(&self) -> i32 {
        i32::from(!self.is_healthy())
    }

    pub fn to_text(&self) -> String {
        let mut out = self.backends.iter().map(Report::to_text).collect::<Vec<_>>().join("\n");

        if self.backends.len() > 1 {
            let failed = self.backends.iter().filter(|r| !r.is_healthy()).count();
            out.push_str(&format!(
                "\n{} backends checked, {failed} failed\n",
                self.backends.len()
            ));
        }

        out
    }
}

/// Diagnose one named backend, or every declared one.
pub async fn run(registry: &Registry, args: &DoctorArgs) -> i32 {
    let subjects: Vec<String> = match &args.backend {
        Some(name) => vec![name.clone()],
        None => registry.declared().into_iter().map(str::to_string).collect(),
    };

    if subjects.is_empty() {
        println!("no secrets backends are declared, so every MCP server uses the default keychain");
        return 0;
    }

    let mut backends = Vec::with_capacity(subjects.len());
    for name in subjects {
        match registry.resolve(&name) {
            Ok(backend) => backends.push(diagnose(&name, &backend).await),
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }

    let diagnosis = Diagnosis { backends };

    match args.format {
        DoctorFormat::Text => print!("{}", diagnosis.to_text()),
        DoctorFormat::Json => match serde_json::to_string_pretty(&diagnosis) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("could not render the report: {e}");
                return 1;
            }
        },
    }

    diagnosis.exit_code()
}

#[cfg(test)]
mod tests {
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
                owner: None,
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
        assert_eq!(report.exit_code(), 0);
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
        assert_eq!(report.exit_code(), 1);
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
        assert_eq!(skipped.exit_code(), 0);

        let failed = Report {
            checks: vec![Check::new("mount", Outcome::failed("gone", "put it back"))],
            ..skipped
        };
        assert!(!failed.is_healthy());
        assert_eq!(failed.exit_code(), 1);
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
        assert_eq!(several.exit_code(), 1);
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
        assert_eq!(healthy.exit_code(), 0);
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
}
