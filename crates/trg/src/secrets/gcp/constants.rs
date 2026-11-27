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

/// Turns user output back on for one call. With `core/user_output_enabled`
/// false, `gcloud` exits 0 and prints neither the payload nor its errors, which
/// would read as an empty secret.
pub const USER_OUTPUT: &str = "--user-output-enabled";

/// Keeps error text on stderr whatever `core/verbosity` says, since a failure
/// is told apart by what it printed.
pub const ERRORS_ONLY: &str = "--verbosity=error";

/// The alias for the newest version, whatever state it is in.
pub const LATEST: &str = "latest";

/// Keeps `gcloud` from logging HTTP traffic, whatever `core/log_http` says.
///
/// With that property on, `gcloud` writes each response to its log files, and a
/// `secrets versions access` response carries the secret in `payload.data`. A
/// command line flag outranks the environment and the properties file, so a
/// user's own `gcloud config set core/log_http true` or `CLOUDSDK_CORE_LOG_HTTP`
/// cannot turn it back on for this call.
pub const NO_LOG_HTTP: &str = "--no-log-http";

/// Stops `gcloud` from writing a log file at all.
///
/// The flag above covers HTTP bodies. `gcloud` also writes everything at debug
/// level to a file under its configuration directory regardless of
/// `--verbosity`, and keeps those files for 30 days by default, so nothing a
/// future release logs about a secret is left on disk either. The cost is that
/// there is no debug log to attach to a bug report from these calls.
pub const DISABLE_FILE_LOGGING: (&str, &str) = ("CLOUDSDK_CORE_DISABLE_FILE_LOGGING", "true");
