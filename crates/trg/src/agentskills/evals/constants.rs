/// Directory, relative to the skill root, that holds the eval suite and its fixtures.
///
/// This directory is the answer key. It must never be staged into a run's workspace:
/// see `runner::is_withheld_from_staging`.
pub const EVAL_SUITE_DIR_NAME: &str = "evals";

pub const EVAL_SUITE_MANIFEST_NAME: &str = "evals.json";

pub const DEFAULT_MAX_FIXTURE_BYTES: u64 = 5 * 1024 * 1024;

pub const FIXTURE_BINARY_SAMPLE_BYTES: usize = 8 * 1024;
