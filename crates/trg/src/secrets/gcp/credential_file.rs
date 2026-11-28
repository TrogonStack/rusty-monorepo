//! The credential file `CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE` names, and the
//! principal `gcloud` would act as if it were used.
//!
//! `gcloud` prefers that file over every stored account, so honouring it blindly
//! would let one config act as different principals on different machines. It
//! is kept only when the principal it names can be read out of the file and is
//! the configured account, which is the same answer `gcloud` itself reaches for
//! these two file types: `client_email` for a service account key, and the
//! service account in `service_account_impersonation_url` for an
//! `external_account` file. Anything else, an `external_account` file that
//! impersonates nobody (the principal is then a federated subject that only a
//! network call to Google resolves) or an `authorized_user` file (a person's
//! refresh token, no email in it), cannot be matched and is refused.
//!
//! Only the three fields needed are read. A service account key also holds a
//! private key, which no value built here carries and no message prints.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::constants::{CREDENTIAL_FILE_OVERRIDE, CREDENTIAL_FILE_SIZE_LIMIT};
use super::GcpAccount;

/// A credential file this backend may hand to `gcloud`, with the principal it
/// was verified to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCredentialFile {
    path: PathBuf,
    principal: String,
}

impl VerifiedCredentialFile {
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialFileError {
    #[error("{CREDENTIAL_FILE_OVERRIDE} names `{}`, which could not be read: {cause}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("{CREDENTIAL_FILE_OVERRIDE} names `{}`, which is larger than {CREDENTIAL_FILE_SIZE_LIMIT} bytes and so not a credential file", path.display())]
    TooLarge { path: PathBuf },

    #[error("{CREDENTIAL_FILE_OVERRIDE} names `{}`, which is not a JSON credential file: {cause}", path.display())]
    NotJson {
        path: PathBuf,
        #[source]
        cause: serde_json::Error,
    },

    #[error(
        "{CREDENTIAL_FILE_OVERRIDE} names `{}`, which names no principal `trg` can check against account `{account}` ({why}); use a service account key, or an external_account file that sets service_account_impersonation_url, or unset it and store the credential with `gcloud auth login --cred-file` for `{account}`",
        path.display()
    )]
    NoPrincipal {
        path: PathBuf,
        account: String,
        why: &'static str,
    },

    #[error(
        "{CREDENTIAL_FILE_OVERRIDE} names `{}`, a credential for `{principal}`, but the configured account is `{account}`; unset it, or fix `account` to match",
        path.display()
    )]
    Mismatch {
        path: PathBuf,
        principal: String,
        account: String,
    },
}

#[derive(Deserialize)]
struct Fields {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_email: Option<String>,
    service_account_impersonation_url: Option<String>,
}

/// The file `value` names, verified to be for `account`.
pub fn verify(value: &Path, account: &GcpAccount) -> Result<VerifiedCredentialFile, CredentialFileError> {
    let unreadable = |cause| CredentialFileError::Unreadable {
        path: value.to_path_buf(),
        cause,
    };
    let file = std::fs::File::open(value).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(CREDENTIAL_FILE_SIZE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > CREDENTIAL_FILE_SIZE_LIMIT {
        return Err(CredentialFileError::TooLarge {
            path: value.to_path_buf(),
        });
    }
    let fields: Fields = serde_json::from_slice(&bytes).map_err(|cause| CredentialFileError::NotJson {
        path: value.to_path_buf(),
        cause,
    })?;

    let no_principal = |why| CredentialFileError::NoPrincipal {
        path: value.to_path_buf(),
        account: account.to_string(),
        why,
    };
    let principal = match fields.kind.as_deref() {
        Some("service_account") => fields
            .client_email
            .filter(|email| is_email(email))
            .ok_or_else(|| no_principal("a service account key without a client_email"))?,
        Some("external_account") => fields
            .service_account_impersonation_url
            .as_deref()
            .and_then(impersonated_email)
            .ok_or_else(|| no_principal("an external_account file that impersonates no service account"))?,
        Some("authorized_user") => return Err(no_principal("an authorized_user file holds no email")),
        _ => return Err(no_principal("a credential of a type `trg` does not accept")),
    };

    if !principal.eq_ignore_ascii_case(account.as_str()) {
        return Err(CredentialFileError::Mismatch {
            path: value.to_path_buf(),
            principal,
            account: account.to_string(),
        });
    }
    Ok(VerifiedCredentialFile {
        path: value.to_path_buf(),
        principal,
    })
}

fn is_email(raw: &str) -> bool {
    !raw.is_empty() && raw.contains('@') && !raw.contains(['/', ':', ' '])
}

/// The service account in
/// `https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/<email>:generateAccessToken`.
fn impersonated_email(url: &str) -> Option<String> {
    let rest = url.strip_suffix(":generateAccessToken")?;
    let email = rest.rsplit_once("/serviceAccounts/")?.1;
    is_email(email).then(|| email.to_string())
}

#[cfg(test)]
#[path = "credential_file_tests.rs"]
mod tests;
