use super::Duration;

/// How long an `op` invocation gets before it is treated as hung.
///
/// `op` normally answers in well under a second, but a stalled 1Password
/// desktop app or a Touch ID prompt nothing is watching for can otherwise
/// block `get`/`current_account` — and, through the latter, `trg doctor` —
/// forever.
pub const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Handed to every `op` call; see [`super::OnePasswordBackend::run`] for why.
pub const OP_NO_CACHE: &str = "--cache=false";

pub const ATTRIBUTES: &str = "type|value|id|purpose|otp|content|size|name";
