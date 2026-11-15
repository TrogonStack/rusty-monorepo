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

mod constants;
use clap::Args;
use constants::{IMPLICIT, OP_SUBTREE_SKIP};
use serde::Serialize;
use tracing::field::Empty;
use tracing::Instrument;

use crate::exit_code::ExitCode;
use crate::output::OutputFormat;
use crate::secrets::onepassword;
use crate::secrets::openbao::{Health, TokenSource};
use crate::secrets::{
    Backend, BackendError, BackendKind, KeychainBackend, OnePasswordBackend, OpenBaoBackend, Registry, SecretsError,
};
use crate::telemetry::semconv::trg::{DOCTOR_HEALTHY, SECRETS_BACKEND_KIND, SECRETS_BACKEND_NAME};

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Check only this backend, as named under `[secrets.backends.<name>]`.
    /// Every declared backend is checked when this is omitted.
    #[arg(long)]
    pub backend: Option<String>,

    /// Output format. Neither format emits a secret value.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
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

    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from_success(self.is_healthy())
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

/// The span one backend's diagnosis runs under. An unhealthy backend is the
/// diagnosis succeeding at its job, so it is recorded as a finding rather
/// than as the span failing.
fn diagnosis_span(name: &str, kind: &str) -> tracing::Span {
    tracing::info_span!(
        "diagnose backend",
        { SECRETS_BACKEND_NAME } = name,
        { SECRETS_BACKEND_KIND } = kind,
        { DOCTOR_HEALTHY } = Empty,
    )
}

pub async fn diagnose(name: &str, backend: &Backend) -> Report {
    let span = diagnosis_span(name, backend.kind());
    let report = async {
        match backend {
            Backend::OpenBao(bao) => openbao(name, bao).await,
            Backend::Keychain(kc) => keychain(name, kc),
            Backend::OnePassword(op) => onepassword(name, op).await,
            #[cfg(test)]
            Backend::Fake(_) => Report {
                backend: name.to_string(),
                kind: backend.kind(),
                target: backend.describe(),
                checks: Vec::new(),
            },
        }
    }
    .instrument(span.clone())
    .await;
    span.record(DOCTOR_HEALTHY, report.is_healthy());
    report
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
        // Building the backend rejects this, so no `OpenBaoBackend` reaches
        // here holding one. Reported rather than panicked over, because a
        // report that dies is worse than one that names something odd.
        TokenSource::Var(crate::config::VarSource::Secret(v)) => format!("{v}"),
        TokenSource::Var(crate::config::VarSource::SecretWithFallbacks(_)) => "a fallback list".to_string(),
    };

    match bao.token_is_readable() {
        Ok(()) => Check::new("token", Outcome::passed(source)),
        // The error names its own source, and the remedy has a line of its
        // own here, so neither is worth repeating alongside it.
        Err(e) => Check::new("token", Outcome::failed(e.report(), e.remedy())),
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
        // and a token that could not be read never reaches it either, so
        // neither says anything about whether the mount is there.
        Err(
            e @ (SecretsError::Unauthorized { .. }
            | SecretsError::Unauthenticated { .. }
            | SecretsError::PermissionDenied(_)),
        ) => vec![
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
        target: kc.describe(),
        checks: vec![
            Check::new("platform", platform),
            Check::new(
                "subtree",
                Outcome::skipped("the keychain does not support listing, so there is nothing to enumerate"),
            ),
        ],
    }
}

/// Two questions, in the order that keeps their remedies apart: does `op` know
/// the account this backend names, and does that account have a session that
/// can read?
///
/// `op` conflates them — a filter it cannot match and an account it cannot
/// unlock both surface as a failed read — and the fixes are a config edit and
/// an unlock prompt respectively, so a typo would otherwise send someone to
/// `op signin` over and over.
async fn onepassword(name: &str, op: &OnePasswordBackend) -> Report {
    let account_ref = op.account();
    let report = |checks| Report {
        backend: name.to_string(),
        kind: "onepassword",
        target: op.describe(),
        checks,
    };

    let known = match op.accounts().await {
        Ok(known) => known,
        Err(err) => {
            return report(vec![
                Check::new(
                    "account",
                    Outcome::failed(
                        err.to_string(),
                        "install the 1Password CLI and add an account, so `op account list` answers",
                    ),
                ),
                Check::new(
                    "session",
                    Outcome::skipped("`op` could not be asked which accounts it knows"),
                ),
                Check::new("subtree", Outcome::skipped(OP_SUBTREE_SKIP)),
            ]);
        }
    };

    let Some(addressed) = onepassword::resolve(account_ref, &known) else {
        let detail = if known.is_empty() {
            format!("`{account_ref}` is not known to `op`, which has no accounts added")
        } else {
            let names: Vec<String> = known.iter().map(ToString::to_string).collect();
            format!(
                "`{account_ref}` is not one of the accounts `op` knows: {}",
                names.join(", ")
            )
        };
        return report(vec![
            Check::new(
                "account",
                Outcome::failed(
                    detail,
                    format!(
                        "point `account` under `[secrets.backends.{name}]` at one of those, \
                         or add the account with `op account add`"
                    ),
                ),
            ),
            Check::new(
                "session",
                Outcome::skipped("there is no account to hold a session, so it was not asked for"),
            ),
            Check::new("subtree", Outcome::skipped(OP_SUBTREE_SKIP)),
        ]);
    };

    let session = match op.current_account().await {
        Err(err) => Outcome::failed(
            err.to_string(),
            format!("unlock the 1Password app, or run `op signin --account {account_ref}`, so the `op` CLI has a session for it"),
        ),
        Ok(current) if !current.state.eq_ignore_ascii_case("ACTIVE") => Outcome::failed(
            format!("`{}` is {}, not ACTIVE", current.name, current.state),
            "reactivate the account in 1Password, or point `account` at one that is active",
        ),
        // A bare sign-in subdomain can name more than one account, and the
        // one listed above is only the first that matched. `op` resolved the
        // same filter itself, so disagreeing ids mean reads land somewhere
        // other than where this report just said they would.
        Ok(current) if !current.id.is_empty() && !addressed.account_uuid.is_empty() && current.id != addressed.account_uuid => {
            Outcome::failed(
                format!(
                    "`op` resolved `{account_ref}` to `{}`, not to {addressed}",
                    current.name
                ),
                "name the account by its full sign-in address or its UUID, since a bare subdomain can match more than one",
            )
        }
        Ok(current) => Outcome::passed(format!("reads go to `{}`", current.name)),
    };

    report(vec![
        Check::new("account", Outcome::passed(format!("`{account_ref}` is {addressed}"))),
        Check::new("session", session),
        Check::new("subtree", Outcome::skipped(OP_SUBTREE_SKIP)),
    ])
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

    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from_success(self.is_healthy())
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
///
/// Declaring nothing is not the same as using nothing: every MCP server then
/// falls to the built-in default, so that is what gets checked. Reporting it as
/// having nothing to check would pass on a platform where the default cannot
/// work at all.
pub async fn diagnose_all(registry: &Registry, only: Option<&str>) -> Result<Diagnosis, BackendError> {
    let names: Vec<String> = match only {
        Some(name) => vec![name.to_string()],
        None => registry.declared().into_iter().map(str::to_string).collect(),
    };

    if names.is_empty() {
        return Ok(Diagnosis {
            backends: vec![diagnose(IMPLICIT, &Registry::default_backend()).await],
        });
    }

    let mut backends = Vec::with_capacity(names.len());
    for name in names {
        match registry.resolve(&name) {
            Ok(backend) => backends.push(diagnose(&name, &backend).await),
            // Naming a backend that was never declared is a mistake in the
            // command, not a finding about a backend.
            Err(e @ (BackendError::Unknown { .. } | BackendError::NoneDeclared { .. })) => return Err(e),
            // Everything else is this backend's own config being wrong, which
            // is the first thing to report about it rather than a reason to
            // stop looking at the others.
            Err(e) => backends.push(unbuildable(&name, registry.kind_of(&name).map(BackendKind::as_str), &e)),
        }
    }
    Ok(Diagnosis { backends })
}

/// A backend that could not be built at all still gets a report, so one broken
/// entry does not hide the state of every entry after it.
fn unbuildable(name: &str, kind: Option<&'static str>, e: &BackendError) -> Report {
    let span = diagnosis_span(name, kind.unwrap_or("unknown"));
    span.record(DOCTOR_HEALTHY, false);
    Report {
        backend: name.to_string(),
        kind: kind.unwrap_or("unknown"),
        target: "nowhere: the backend could not be built from its config".to_string(),
        checks: vec![Check::new(
            "config",
            Outcome::failed(
                e.to_string(),
                "fix the declaration this names, then run `trg doctor` again",
            ),
        )],
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub async fn run(registry: &Registry, args: &DoctorArgs) -> ExitCode {
    let diagnosis = match diagnose_all(registry, args.backend.as_deref()).await {
        Ok(diagnosis) => diagnosis,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::Failed;
        }
    };

    match args.output_format {
        OutputFormat::Text => print!("{}", diagnosis.to_text()),
        OutputFormat::Json => match serde_json::to_string_pretty(&diagnosis) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("could not render the report: {e}");
                return ExitCode::Failed;
            }
        },
    }

    diagnosis.exit_code()
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
