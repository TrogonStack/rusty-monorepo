/// The status recorded on a run stopped for going past one of its caps.
pub const RUN_STATUS_BUDGET_EXHAUSTED: &str = "budget_exhausted";

pub const FAILURE_KIND_RUNNER: &str = "runner";

/// The failure kind recorded on a run skipped because the case asked for a control the
/// chosen runner does not offer.
pub const FAILURE_KIND_UNSUPPORTED: &str = "unsupported";

pub const VCS_DIRS_WITHHELD_FROM_STAGING: &[&str] = &[".git", ".jj", ".hg", ".svn"];
