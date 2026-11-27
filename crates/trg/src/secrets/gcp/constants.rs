use super::Duration;

/// How long a `gcloud` invocation gets before it is treated as hung.
///
/// Longer than `op`'s budget because `gcloud` is a Python program that starts
/// slowly and makes a network call per command. A stalled browser
/// reauthentication prompt is what this bounds, so a read cannot block
/// forever, and through `trg doctor` neither can the diagnosis.
pub const GCLOUD_TIMEOUT: Duration = Duration::from_secs(30);

/// The most a secret id may hold, per Secret Manager.
pub const SECRET_ID_MAX_LEN: usize = 255;

/// Handed to every `gcloud` call so nothing can stop to ask a question on a
/// terminal nobody is watching. A failed prompt becomes an error instead.
pub const QUIET: &str = "--quiet";

/// The settings that make `gcloud` act as an identity other than the one
/// `--account` names, as the environment variables that carry them.
///
/// `gcloud` resolves a credential in a fixed order: an access token, an access
/// token file, a credential file override, and only then the stored account
/// `--account` selects. Impersonation then swaps the result for another
/// principal. A CI job that ran `google-github-actions/auth` sets the
/// credential file one, so the same config would act as a different principal
/// there than on a laptop.
///
/// Each is set to the empty string rather than removed. `gcloud` reads an
/// environment value before the active configuration's properties file and
/// treats an empty one as unset, so this also shadows the same setting made
/// through `gcloud config set`.
pub const IDENTITY_OVERRIDES: [&str; 4] = [
    "CLOUDSDK_AUTH_ACCESS_TOKEN",
    "CLOUDSDK_AUTH_ACCESS_TOKEN_FILE",
    "CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE",
    "CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT",
];
