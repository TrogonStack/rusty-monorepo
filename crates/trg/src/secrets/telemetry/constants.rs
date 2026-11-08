/// `error.type` for a child process that exited with a status other than 0,
/// the same value the root span records for `trg`'s own exit.
pub const NONZERO_EXIT: &str = "nonzero_exit";

/// `error.type` for a child process killed before it reported a status.
pub const SIGNALED: &str = "signaled";

/// `error.type` for a child process that outlived its budget.
pub const TIMEOUT: &str = "timeout";

/// Methods the HTTP conventions name; anything else is reported as `_OTHER`
/// with the original kept beside it.
pub const KNOWN_METHODS: [&str; 9] = [
    "CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE",
];

pub const OTHER_METHOD: &str = "_OTHER";
