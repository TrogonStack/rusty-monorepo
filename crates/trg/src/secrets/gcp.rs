//! Google Cloud Secret Manager backend, driven through an authenticated
//! `gcloud` CLI.
//!
//! # Why we shell out to `gcloud` instead of calling the API
//!
//! A native client would need an OAuth stack for Application Default
//! Credentials, a gRPC or REST client for Secret Manager, and a decision about
//! which credential source wins when several exist. `gcloud` already is all of
//! that, and the session it holds is the one a developer's shell, and a CI job
//! that ran `google-github-actions/auth`, already trust. Shelling out reuses it
//! with nothing new to authenticate or store, which is the same trade
//! [`super::onepassword`] makes for `op` and [`super::keychain`] makes for
//! `security`, and it adds no dependency to this crate.
//!
//! # Why the project is required and never inferred
//!
//! `gcloud` resolves a missing `--project` from `core/project`, which is
//! machine-local state the config does not say. The same
//! `[secrets.backends.<name>]` would then read one project on a laptop and
//! another in CI, and a write could land somewhere nobody meant. [`GcpProject`]
//! is therefore mandatory and every invocation carries `--project`.
//!
//! The account is mandatory for the same reason: `gcloud`'s active account is
//! machine-local state too, so every invocation carries `--account` and a
//! machine signed into several identities always acts as the one the config
//! names.
//!
//! `--account` alone does not hold that. `gcloud` prefers an access token, an
//! access token file or a credential file override over the account it was
//! told to use, and impersonation replaces the result with another principal.
//! Every invocation therefore blanks those settings, so the identity is the
//! stored credential of the configured account or `gcloud` refuses. A CI job
//! that authenticates through a credential file must store it for the account
//! first, as `gcloud auth login --cred-file` does.
//!
//! # Addressing
//!
//! A value is addressed by its [`GcpSecretId`] alone. A Secret Manager secret is
//! one opaque payload, and the payloads are versions rather than fields, so
//! there is no `key` for a reference to carry. Reads take the `latest` version.
//!
//! # What this writes, and what it does not
//!
//! The one write is `gcloud secrets versions add`, with the value on stdin so it
//! never reaches argv. It never creates a secret. The container (its
//! replication policy, IAM, labels, rotation) is infrastructure that something
//! like Terraform owns, and a tool that quietly created one would also decide
//! those settings. A container that is missing is reported as such rather than
//! made.
//!
//! # Why reads go through `--format=get(payload.data)`
//!
//! Plain `versions access` formats the payload as UTF-8 text, and `gcloud`'s own
//! help warns that this can corrupt binary secrets. Asking for the base64 form
//! and decoding it here returns the stored bytes exactly, including a trailing
//! newline, which a PEM private key ends with and a client may require.

mod constants;
use constants::{ERRORS_ONLY, GCLOUD_TIMEOUT, IDENTITY_OVERRIDES, LATEST, QUIET, SECRET_ID_MAX_LEN, USER_OUTPUT};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::{alphabet, Engine};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::Instrument;

use super::telemetry::{CliSpan, TIMEOUT};
use super::{BackendKind, SecretPath, SecretsError};
use crate::telemetry::propagation;

/// The `gcloud` subcommands this backend drives, as a closed set so the one
/// that lands on a span is always a fixed phrase and never an argument.
#[derive(Debug, Clone, Copy)]
enum GcloudCommand {
    VersionsAccess,
    VersionsAdd,
    SecretsDescribe,
    SecretsList,
    AuthList,
}

impl GcloudCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::VersionsAccess => "secrets versions access",
            Self::VersionsAdd => "secrets versions add",
            Self::SecretsDescribe => "secrets describe",
            Self::SecretsList => "secrets list",
            Self::AuthList => "auth list",
        }
    }

    fn words(self) -> &'static [&'static str] {
        match self {
            Self::VersionsAccess => &["secrets", "versions", "access"],
            Self::VersionsAdd => &["secrets", "versions", "add"],
            Self::SecretsDescribe => &["secrets", "describe"],
            Self::SecretsList => &["secrets", "list"],
            Self::AuthList => &["auth", "list"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GcpProjectError {
    #[error("Google Cloud project id must not be empty")]
    Empty,

    #[error(
        "Google Cloud project id `{0}` must be 6 to 30 characters of lowercase letters, digits and `-`, starting with a letter and not ending with `-`; a project number or display name is not an id"
    )]
    Shape(String),
}

/// The Google Cloud project id a backend reads and writes secrets in.
///
/// An id and not a number or a display name, because those are not what
/// `--project` takes and a typo there should be refused while the config is in
/// view. The id grammar also rules out a leading `-`, which `gcloud` would read
/// as a flag.
#[derive(Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct GcpProject(String);

impl GcpProject {
    pub fn parse(raw: &str) -> Result<Self, GcpProjectError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(GcpProjectError::Empty);
        }
        let bytes = raw.as_bytes();
        let well_formed = (6..=30).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes[bytes.len() - 1] != b'-'
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if !well_formed {
            return Err(GcpProjectError::Shape(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GcpProject {
    type Error = GcpProjectError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for GcpProject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GcpProject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GcpProject({:?})", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GcpAccountError {
    #[error("Google Cloud account must not be empty")]
    Empty,

    /// `gcloud` would read this as a flag rather than as the account to use.
    #[error("Google Cloud account `{0}` must not start with `-`")]
    Dash(String),
}

/// Which `gcloud` identity a backend acts as, as `gcloud auth list` names it:
/// a user email or a service account email.
///
/// Structurally validated only. Whether `gcloud` holds credentials for it is
/// what `trg doctor` asks.
#[derive(Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct GcpAccount(String);

impl GcpAccount {
    pub fn parse(raw: &str) -> Result<Self, GcpAccountError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(GcpAccountError::Empty);
        }
        if raw.starts_with('-') {
            return Err(GcpAccountError::Dash(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GcpAccount {
    type Error = GcpAccountError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for GcpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GcpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GcpAccount({:?})", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GcpSecretIdError {
    #[error("Secret Manager secret id must not be empty")]
    Empty,

    #[error("Secret Manager secret id `{0}` is longer than {SECRET_ID_MAX_LEN} characters")]
    TooLong(String),

    #[error("Secret Manager secret id `{0}` may hold only ASCII letters, digits, `-` and `_`")]
    Characters(String),

    /// Legal to Secret Manager, refused here because the id is handed to
    /// `gcloud` as a positional argument, where a leading `-` reads as a flag.
    #[error("Secret Manager secret id `{0}` must not start with `-`")]
    Dash(String),
}

/// A Secret Manager secret id, the short name and not the
/// `projects/<p>/secrets/<id>` resource name.
///
/// A secret id is also a valid [`SecretPath`] (letters, digits, `-` and `_`
/// contain no `/`, `#` or dot segment), which is how errors that carry a path
/// can name one without inventing a placeholder.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GcpSecretId(String);

impl GcpSecretId {
    pub fn parse(raw: &str) -> Result<Self, GcpSecretIdError> {
        if raw.is_empty() {
            return Err(GcpSecretIdError::Empty);
        }
        if raw.len() > SECRET_ID_MAX_LEN {
            return Err(GcpSecretIdError::TooLong(raw.to_string()));
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(GcpSecretIdError::Characters(raw.to_string()));
        }
        if raw.starts_with('-') {
            return Err(GcpSecretIdError::Dash(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This id as the path errors that name one carry.
    pub fn as_path(&self) -> SecretPath {
        SecretPath::parse(&self.0).expect("a secret id holds only characters a path allows")
    }
}

impl fmt::Display for GcpSecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GcpSecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GcpSecretId({:?})", self.0)
    }
}

/// The number of one Secret Manager version, as `versions add` reports it.
#[derive(Clone, PartialEq, Eq)]
pub struct GcpVersion(String);

impl GcpVersion {
    /// The version out of `projects/<n>/secrets/<id>/versions/<v>`. Only a
    /// run of digits is taken, since the value goes to `gcloud` as an argument.
    fn from_resource_name(name: &str) -> Option<Self> {
        let number = name.rsplit('/').next()?;
        (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then(|| Self(number.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GcpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GcpVersion({:?})", self.0)
    }
}

/// One value's address in this backend: the secret whose latest version it is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GcpReference {
    secret: GcpSecretId,
}

impl GcpReference {
    pub fn new(secret: GcpSecretId) -> Self {
        Self { secret }
    }

    pub fn secret(&self) -> &GcpSecretId {
        &self.secret
    }
}

/// One row of `gcloud auth list`: an identity `gcloud` on this machine holds
/// credentials for.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GcloudAccount {
    #[serde(default)]
    pub account: String,
}

/// What a failed `gcloud` call said it was, read from the text it printed.
///
/// `gcloud` exits 1 for everything and names the cause only in stderr, so the
/// tokens here are the ones its Secret Manager errors carry: the gRPC status
/// name on the API errors, which has been stable across releases.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    NotFound,
    Denied,
    Other,
}

fn classify(stderr: &str) -> Failure {
    if stderr.contains("PERMISSION_DENIED") || stderr.contains("does not have permission") {
        Failure::Denied
    } else if stderr.contains("NOT_FOUND") || stderr.contains("not found") || stderr.contains("was not found") {
        Failure::NotFound
    } else {
        Failure::Other
    }
}

/// Whether a failed `versions access` said the version exists but is disabled
/// or destroyed, which is `FAILED_PRECONDITION: ... is in DISABLED state`.
///
/// Only those two states. Any other `FAILED_PRECONDITION` is not a statement
/// about the version and stays an error. A destroyed version cannot come back,
/// so adding one is the only way forward, and that is the same path as a
/// disabled one.
fn is_unreadable_state(stderr: &str) -> bool {
    stderr.contains("FAILED_PRECONDITION")
        && (stderr.contains("is in DISABLED state") || stderr.contains("is in DESTROYED state"))
}

#[derive(Clone)]
pub struct GcpBackend {
    project: GcpProject,
    account: GcpAccount,
    /// The `gcloud` executable to drive. Held as data rather than hardcoded at
    /// the call site so tests can drive a scripted one and pin the exact argv.
    bin: PathBuf,
    /// How long one `gcloud` invocation gets; overridden in tests so a hung
    /// stub does not cost the suite real wall-clock time.
    timeout: Duration,
}

impl GcpBackend {
    pub fn new(project: GcpProject, account: GcpAccount) -> Self {
        Self {
            project,
            account,
            bin: PathBuf::from("gcloud"),
            timeout: GCLOUD_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_bin(mut self, bin: impl AsRef<std::path::Path>) -> Self {
        self.bin = bin.as_ref().to_path_buf();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn project(&self) -> &GcpProject {
        &self.project
    }

    pub fn account(&self) -> &GcpAccount {
        &self.account
    }

    /// Where this backend puts things, phrased for a person reading command
    /// output.
    pub fn describe(&self) -> String {
        format!(
            "Google Cloud Secret Manager via `gcloud` (project `{}`, account `{}`)",
            self.project, self.account
        )
    }

    /// The bytes of the `latest` version of a secret, or `None` when there is
    /// nothing to read: the secret does not exist, or its newest version is
    /// not one that can be read. `latest` is the newest version in any state, so
    /// a disabled or destroyed one answers `FAILED_PRECONDITION` rather than
    /// falling back to an older one. Neither is a reason to fail: a new version
    /// is the way out of both.
    pub async fn get_latest(&self, secret: &GcpSecretId) -> Result<Option<SecretString>, SecretsError> {
        self.access(secret, LATEST).await
    }

    /// The bytes of one specific version, which unlike `latest` is strongly
    /// consistent right after the version was added. `None` as for
    /// [`Self::get_latest`].
    pub async fn get_version(
        &self,
        secret: &GcpSecretId,
        version: &GcpVersion,
    ) -> Result<Option<SecretString>, SecretsError> {
        self.access(secret, version.as_str()).await
    }

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The payload is a secret and a UTF-8 error carries the bytes it rejected, so neither decode error may travel into a message"
        )
    )]
    async fn access(&self, secret: &GcpSecretId, version: &str) -> Result<Option<SecretString>, SecretsError> {
        let out = self
            .run(
                GcloudCommand::VersionsAccess,
                &[version, &format!("--secret={secret}"), "--format=get(payload.data)"],
                None,
            )
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if is_unreadable_state(stderr) {
                return Ok(None);
            }
            return match classify(stderr) {
                Failure::NotFound => Ok(None),
                Failure::Denied => Err(self.denied(GcloudCommand::VersionsAccess, stderr)),
                Failure::Other => Err(self.failed(GcloudCommand::VersionsAccess, stderr)),
            };
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let bytes = payload_engine()
            .decode(stdout.trim())
            .map_err(|_| self.malformed(secret, "the payload was not the base64 `gcloud` was asked for"))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| self.malformed(secret, "the stored value is not valid UTF-8, which `trg` cannot carry"))?;
        Ok(Some(SecretString::from(text)))
    }

    /// Add `value` as a new version of a secret that already exists.
    ///
    /// The value goes to `gcloud` on stdin and is written exactly as given.
    /// A secret that is not there is [`SecretsError::MissingContainer`],
    /// never a reason to create one.
    ///
    /// Answers with the version that was created, so a readback can name it
    /// rather than ask `latest`, an alias that may lag behind the write.
    pub async fn add_version(&self, secret: &GcpSecretId, value: &SecretString) -> Result<GcpVersion, SecretsError> {
        let out = self
            .run(
                GcloudCommand::VersionsAdd,
                &[secret.as_str(), "--data-file=-", "--format=value(name)"],
                Some(value),
            )
            .await?;

        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return GcpVersion::from_resource_name(stdout.trim()).ok_or_else(|| {
                SecretsError::Unavailable(format!(
                    "gcloud {} succeeded but did not say which version it created (project `{}`)",
                    GcloudCommand::VersionsAdd.as_str(),
                    self.project
                ))
            });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        match classify(stderr) {
            Failure::NotFound => Err(self.missing_container(secret)),
            Failure::Denied => Err(self.denied(GcloudCommand::VersionsAdd, stderr)),
            Failure::Other => Err(self.failed(GcloudCommand::VersionsAdd, stderr)),
        }
    }

    /// Whether the secret exists at all, as opposed to existing with no
    /// version. A metadata read, which Secret Manager does not bill.
    pub async fn secret_exists(&self, secret: &GcpSecretId) -> Result<bool, SecretsError> {
        let out = self
            .run(
                GcloudCommand::SecretsDescribe,
                &[secret.as_str(), "--format=value(name)"],
                None,
            )
            .await?;
        if out.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        match classify(stderr) {
            Failure::NotFound => Ok(false),
            Failure::Denied => Err(self.denied(GcloudCommand::SecretsDescribe, stderr)),
            Failure::Other => Err(self.failed(GcloudCommand::SecretsDescribe, stderr)),
        }
    }

    /// Every identity `gcloud` holds credentials for, for `trg doctor` to
    /// resolve the configured account against. Reads local state, so it
    /// answers without the network.
    pub async fn accounts(&self) -> Result<Vec<GcloudAccount>, SecretsError> {
        let out = self.run_global(GcloudCommand::AuthList, &["--format=json"]).await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "gcloud auth list failed: {}",
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("gcloud auth list did not return valid JSON: {e}")))
    }

    /// One `secrets list` of at most one entry: the project answers, the
    /// identity may use Secret Manager in it, and nothing is read that was not
    /// already listable. Fails the same way for a project that does not exist
    /// and one this identity cannot see, which is as much as `gcloud` says.
    pub async fn probe_project(&self) -> Result<(), SecretsError> {
        let out = self
            .run(GcloudCommand::SecretsList, &["--limit=1", "--format=value(name)"], None)
            .await?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        match classify(stderr) {
            Failure::Denied => Err(self.denied(GcloudCommand::SecretsList, stderr)),
            _ => Err(self.failed(GcloudCommand::SecretsList, stderr)),
        }
    }

    fn failed(&self, command: GcloudCommand, stderr: &str) -> SecretsError {
        SecretsError::Unavailable(format!(
            "gcloud {} failed (project `{}`): {stderr}",
            command.as_str(),
            self.project
        ))
    }

    fn denied(&self, command: GcloudCommand, stderr: &str) -> SecretsError {
        SecretsError::PermissionDenied(format!(
            "gcloud {} was refused (project `{}`): {stderr}",
            command.as_str(),
            self.project
        ))
    }

    fn malformed(&self, secret: &GcpSecretId, cause: &str) -> SecretsError {
        SecretsError::Malformed {
            path: secret.as_path(),
            cause: cause.to_string(),
            raw: None,
        }
    }

    pub(crate) fn missing_container(&self, secret: &GcpSecretId) -> SecretsError {
        SecretsError::MissingContainer {
            project: self.project.to_string(),
            secret: secret.to_string(),
        }
    }

    /// [`Self::run_global`] aimed at this backend's project and account.
    async fn run(
        &self,
        command: GcloudCommand,
        args: &[&str],
        stdin: Option<&SecretString>,
    ) -> Result<std::process::Output, SecretsError> {
        let project = format!("--project={}", self.project);
        let mut all: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        all.push(project);
        all.push(format!("--account={}", self.account));
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        self.invoke(command, &all, stdin).await
    }

    /// For the one command that is about the machine and not a project.
    async fn run_global(&self, command: GcloudCommand, args: &[&str]) -> Result<std::process::Output, SecretsError> {
        self.invoke(command, args, None).await
    }

    /// Runs `gcloud <command> <args> --quiet` under a CLI client span that
    /// names the subcommand and none of the arguments, which address the
    /// secret. The value, when there is one, goes in on stdin.
    async fn invoke(
        &self,
        command: GcloudCommand,
        args: &[&str],
        stdin: Option<&SecretString>,
    ) -> Result<std::process::Output, SecretsError> {
        let call = CliSpan::start(BackendKind::GcpSecretManager, "gcloud", command.as_str());
        let invocation = || format!("{} {}", command.as_str(), args.join(" "));

        let mut process = Command::new(&self.bin);
        process
            .args(command.words())
            .args(args)
            .args([QUIET, USER_OUTPUT, ERRORS_ONLY])
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for name in IDENTITY_OVERRIDES {
            process.env(name, "");
        }
        call.span().in_scope(|| propagation::inject_tokio_command(&mut process));

        let mut child = process.spawn().map_err(|e| {
            let error = SecretsError::Transport(format!("gcloud {}: {e}", invocation()));
            call.failed(error.error_type());
            error
        })?;
        call.spawned(child.id());

        // The value is written and the pipe closed before waiting, so `gcloud`
        // sees end of input and does not wait on a stream that never ends.
        // Writing is inside the timeout with the wait: a `gcloud` that stalls
        // before it reads leaves a value larger than the pipe unwritten, and a
        // timeout that began afterwards would never start.
        //
        // A broken pipe means `gcloud` exited without reading, as it does when
        // the secret is not there. Its exit status and stderr say why, so the
        // wait reports that instead of the pipe.
        let mut pipe = child.stdin.take();
        let exchange = async {
            if let (Some(value), Some(pipe)) = (stdin, pipe.as_mut()) {
                let wrote = async {
                    pipe.write_all(value.expose_secret().as_bytes()).await?;
                    pipe.shutdown().await
                }
                .await;
                match wrote {
                    Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(Exchange::Send(e)),
                    _ => {}
                }
            }
            drop(pipe.take());
            child.wait_with_output().await.map_err(Exchange::Wait)
        };
        let waited = tokio::time::timeout(self.timeout, exchange)
            .instrument(call.span().clone())
            .await;
        match waited {
            Ok(Ok(output)) => {
                call.exited(&output.status);
                Ok(output)
            }
            Ok(Err(Exchange::Send(e))) => {
                let error =
                    SecretsError::Transport(format!("gcloud {}: could not send the value: {e}", command.as_str()));
                call.failed(error.error_type());
                Err(error)
            }
            Ok(Err(Exchange::Wait(e))) => {
                let error = SecretsError::Transport(format!("gcloud {}: {e}", invocation()));
                call.failed(error.error_type());
                Err(error)
            }
            Err(_) => {
                call.failed(TIMEOUT);
                Err(SecretsError::Transport(format!(
                    "gcloud {} timed out after {:?}; is `gcloud` waiting on a reauthentication?",
                    command.as_str(),
                    self.timeout
                )))
            }
        }
    }
}

/// Which half of one invocation failed, so each keeps its own message.
enum Exchange {
    Send(std::io::Error),
    Wait(std::io::Error),
}

/// `gcloud` prints `payload.data` as URL-safe base64, and which of the padded
/// and unpadded forms it uses is not something to depend on.
fn payload_engine() -> GeneralPurpose {
    GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

#[cfg(test)]
#[path = "gcp_tests.rs"]
mod tests;
