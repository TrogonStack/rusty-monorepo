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
