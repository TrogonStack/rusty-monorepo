//! Signing a file for git, from the first address that can.
//!
//! The signer is chosen by the public key git names, never by default, and its
//! addresses are tried in the order the config lists them. An address that
//! cannot be reached, or that rejected the credentials it was given, is passed
//! over with a warning so a signer that has quietly stopped answering is seen.
//! An address that answered wrongly, or that holds the key and declined to sign,
//! stops the attempt, so no other address can be used to get around a refusal.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

use tracing::field::Empty;
use tracing::Instrument;

use super::agent::{self, AgentSigning};
use super::constants::{SIGNATURE_SUFFIX, SSH_KEYGEN};
use super::key_file::{KeyFile, KeyFileError};
use super::request::{RequestError, SignRequest};
use super::sshsig::{self, Signature};
use super::transit_key;
use crate::config::signers::{NoSignerForKeyError, PublicKey, Signer, SignerAddress, Signers, TransitAddress};
use crate::config::{self, ConfigError};
use crate::secrets::telemetry::record_error;
use crate::secrets::{Backend, OpenBaoBackend, Registry, SecretsError};
use crate::telemetry::semconv::trg::{GIT_SSH_SIGNER_NAME, SECRETS_ANSWERED_BY, SECRETS_FALLBACK_USED};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("{0}")]
    Request(#[from] RequestError),

    #[error("{0}")]
    Config(#[from] ConfigError),

    #[error("{0}")]
    KeyFile(#[from] KeyFileError),

    #[error("{0}")]
    NoSigner(#[from] NoSignerForKeyError),

    #[error("Cannot open {} for signing: {}", path.display(), reason(source))]
    Input {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot write signature to {}: {}", path.display(), reason(source))]
    Output {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// An address answered in a way that must not be routed around.
    #[error("signer `{signer}` stopped at {address}: {cause}")]
    Stopped {
        signer: String,
        address: String,
        cause: String,
    },

    #[error("{}", exhausted(signer, attempts))]
    Exhausted { signer: String, attempts: Vec<String> },
}

fn read_input(file: &Path) -> Result<Vec<u8>, SignError> {
    std::fs::read(file).map_err(|source| SignError::Input {
        path: file.to_path_buf(),
        source,
    })
}

fn write_signature(output: &Path, armored: impl AsRef<[u8]>) -> std::io::Result<()> {
    std::fs::write(output, armored)
}

fn reason(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_string()
}

fn exhausted(signer: &str, attempts: &[String]) -> String {
    let mut out = format!("signer `{signer}` could not sign: no address answered");
    for attempt in attempts {
        out.push_str("\n  - ");
        out.push_str(attempt);
    }
    out
}

impl SignError {
    /// What `ssh-keygen` exits with for the same failure: `1` for a command
    /// line it would have printed usage for, `255` for everything it reports
    /// after starting work.
    pub fn exit_status(&self) -> u8 {
        match self {
            Self::Request(
                RequestError::MissingNamespace
                | RequestError::MissingKey
                | RequestError::MissingFile
                | RequestError::Unrecognized(_),
            ) => 1,
            _ => 255,
        }
    }

    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Request(_) => "request",
            Self::Config(e) => e.error_type(),
            Self::KeyFile(_) => "key_file",
            Self::NoSigner(_) => "no_signer",
            Self::Input { .. } => "input",
            Self::Output { .. } => "output",
            Self::Stopped { .. } => "stopped",
            Self::Exhausted { .. } => "exhausted",
        }
    }
}

enum Attempt {
    Signed,
    /// This address could not be asked, or refused the credentials it was
    /// given; the next one may be.
    Declined(String),
    Stopped(String),
}

pub struct SshSigner {
    signers: Signers,
    registry: Registry,
}

impl SshSigner {
    pub fn new(signers: Signers, registry: Registry) -> Self {
        Self { signers, registry }
    }

    pub fn load() -> Result<Self, ConfigError> {
        let (signers, secrets) = config::load_signers()?;
        Ok(Self::new(signers, Registry::new(secrets)))
    }

    /// Sign every file of the request, each to its own `<file>.sig`.
    pub async fn sign(&self, request: &SignRequest) -> Result<(), SignError> {
        let key_file = KeyFile::read(&request.key_file)?;
        let signer = self.signers.select(key_file.key())?;
        for file in &request.files {
            self.sign_file(signer, &key_file, request, file).await?;
        }
        Ok(())
    }

    async fn sign_file(
        &self,
        signer: &Signer,
        key_file: &KeyFile,
        request: &SignRequest,
        file: &Path,
    ) -> Result<(), SignError> {
        let message = read_input(file)?;
        let output = signature_path(file);
        if output.exists() && !overwrite_approved(&output) {
            return Ok(());
        }

        let span = tracing::info_span!(
            "sign git ssh signature",
            { GIT_SSH_SIGNER_NAME } = signer.name().as_str(),
            { SECRETS_ANSWERED_BY } = Empty,
            { SECRETS_FALLBACK_USED } = Empty,
            "error.type" = Empty,
            "otel.status_code" = Empty,
        );
        let result = self
            .walk(signer, key_file, request, file, &output, &message, &span)
            .instrument(span.clone())
            .await;
        if let Err(e) = &result {
            record_error(&span, e.error_type());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn walk(
        &self,
        signer: &Signer,
        key_file: &KeyFile,
        request: &SignRequest,
        file: &Path,
        output: &Path,
        message: &[u8],
        span: &tracing::Span,
    ) -> Result<(), SignError> {
        let mut attempts: Vec<String> = Vec::new();
        for (index, address) in signer.addresses().enumerate() {
            let attempt = match address {
                SignerAddress::Transit(transit) => self.transit(signer, transit, request, file, output, message).await,
                SignerAddress::SshAgent(socket) => {
                    agent_attempt(socket.path(), signer, key_file, request, message, output)
                }
            };
            match attempt {
                Attempt::Signed => {
                    span.record(SECRETS_ANSWERED_BY, address.label());
                    span.record(SECRETS_FALLBACK_USED, index > 0);
                    if index > 0 {
                        say(&fallback_notice(signer, address, &attempts));
                    }
                    return Ok(());
                }
                Attempt::Declined(why) => attempts.push(format!("{}: {why}", address.describe())),
                Attempt::Stopped(cause) => {
                    return Err(SignError::Stopped {
                        signer: signer.name().to_string(),
                        address: address.describe(),
                        cause,
                    })
                }
            }
        }
        Err(SignError::Exhausted {
            signer: signer.name().to_string(),
            attempts,
        })
    }

    async fn transit(
        &self,
        signer: &Signer,
        address: &TransitAddress,
        request: &SignRequest,
        file: &Path,
        output: &Path,
        message: &[u8],
    ) -> Attempt {
        let backend = match self.registry.resolve(&address.backend) {
            Ok(Backend::OpenBao(backend)) => backend,
            Ok(_) => return Attempt::Stopped(format!("backend `{}` is not an openbao backend", address.backend)),
            Err(e) if e.allows_fallback() => return Attempt::Declined(e.to_string()),
            Err(e) => return Attempt::Stopped(e.to_string()),
        };

        match check_key(&backend, address, signer.public_key()).await {
            Ok(()) => {}
            Err(attempt) => return attempt,
        }

        say(&format!("Signing file {}", file.display()));
        let data = sshsig::signed_data(&request.namespace, request.options.hash(), message);
        let raw = match backend
            .transit_sign(
                address.mount.as_str(),
                address.key.as_str(),
                &data,
                transit_key::sign_parameters(signer.public_key()),
            )
            .await
        {
            Ok(raw) => raw,
            Err(e) => return classify(&e),
        };
        let signature = match Signature::new(signer.public_key(), &raw) {
            Ok(signature) => signature,
            Err(e) => return Attempt::Stopped(e.to_string()),
        };
        if !sshsig::verify(signer.public_key(), &data, &signature) {
            return Attempt::Stopped(
                "the signature Transit returned does not verify against the signer's `public_key`, so it was not written"
                    .to_string(),
            );
        }
        let armored = sshsig::armor(
            signer.public_key(),
            &request.namespace,
            request.options.hash(),
            &signature,
        );
        match write_signature(output, &armored) {
            Ok(()) => {
                say(&format!("Write signature to {}", output.display()));
                Attempt::Signed
            }
            Err(e) => Attempt::Stopped(
                SignError::Output {
                    path: output.to_path_buf(),
                    source: e,
                }
                .to_string(),
            ),
        }
    }
}

/// The public key Transit signs with must be the one declared for the signer.
///
/// A mismatch is a stop and never a fall-through: the signature would verify
/// as a different key than the one git asked for, so trying another address
/// would only hide that the config points at the wrong key.
async fn check_key(backend: &OpenBaoBackend, address: &TransitAddress, expected: &PublicKey) -> Result<(), Attempt> {
    let found = match backend
        .transit_public_key(address.mount.as_str(), address.key.as_str())
        .await
    {
        Ok(Some(found)) => found,
        Ok(None) => {
            return Err(Attempt::Stopped(format!(
                "the Transit key `{}` does not exist under mount `{}`",
                address.key.as_str(),
                address.mount.as_str()
            )))
        }
        Err(e) => return Err(classify(&e)),
    };
    let actual = match transit_key::public_key(&found) {
        Ok(actual) => actual,
        Err(e) => return Err(Attempt::Stopped(e.to_string())),
    };
    if &actual != expected {
        return Err(Attempt::Stopped(format!(
            "the public key of the Transit key is not the `public_key` declared for the signer; \
             the Transit key signs as `{}`",
            actual.to_line()
        )));
    }
    Ok(())
}

fn classify(error: &SecretsError) -> Attempt {
    if error.allows_fallback() {
        Attempt::Declined(error.to_string())
    } else {
        Attempt::Stopped(error.to_string())
    }
}

fn agent_attempt(
    socket: &Path,
    signer: &Signer,
    key_file: &KeyFile,
    request: &SignRequest,
    message: &[u8],
    output: &Path,
) -> Attempt {
    let signed = agent::require_key(socket, signer.public_key()).and_then(|()| {
        agent::sign(&AgentSigning {
            socket,
            namespace: request.namespace.as_str(),
            key_file: key_file.path(),
            options: request.options.raw(),
            message,
        })
    });
    let armored = match signed {
        Ok(armored) => armored,
        Err(e) if e.allows_fallback() => return Attempt::Declined(e.to_string()),
        Err(e) => return Attempt::Stopped(e.to_string()),
    };
    match write_signature(output, &armored) {
        Ok(()) => {
            say(&format!("Write signature to {}", output.display()));
            Attempt::Signed
        }
        Err(e) => Attempt::Stopped(
            SignError::Output {
                path: output.to_path_buf(),
                source: e,
            }
            .to_string(),
        ),
    }
}

fn signature_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(SIGNATURE_SUFFIX);
    PathBuf::from(name)
}

/// `ssh-keygen` asks before replacing a signature, and reads the answer from
/// standard input, so an input that ends is a no.
fn overwrite_approved(output: &Path) -> bool {
    say(&format!("{} already exists.", output.display()));
    prompt("Overwrite (y/n)? ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim_start().starts_with(['y', 'Y'])
}

fn fallback_notice(signer: &Signer, answered: &SignerAddress, attempts: &[String]) -> String {
    let mut out = format!(
        "warning: signer `{}` could not sign with {}, so {} signed",
        signer.name(),
        signer
            .addresses()
            .next()
            .map(SignerAddress::describe)
            .unwrap_or_default(),
        answered.describe()
    );
    for attempt in attempts {
        out.push_str("\n  - ");
        out.push_str(attempt);
    }
    out
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "These lines are the signing program's reply on stderr, which git captures and shows; tracing must not replace that output"
    )
)]
fn say(line: &str) {
    eprintln!("{line}");
}

fn prompt(text: &str) {
    eprint!("{text}");
    std::io::stderr().flush().ok();
}

/// Replace this process with the real `ssh-keygen`, which keeps the arguments,
/// streams and exit status exactly. Only a failure to start it returns.
pub fn pass_through(args: &[OsString]) -> u8 {
    let error = std::process::Command::new(SSH_KEYGEN).args(args).exec();
    say(&format!("could not run `{SSH_KEYGEN}`: {error}"));
    255
}

/// Sign one request and report the way the signing program does.
///
/// Returns the process status to exit with.
pub async fn run(request: Result<SignRequest, RequestError>) -> u8 {
    let result = async {
        let request = request?;
        let signer = SshSigner::load()?;
        signer.sign(&request).await
    }
    .await;
    match result {
        Ok(()) => 0,
        Err(e) => {
            say(&e.to_string());
            e.exit_status()
        }
    }
}
