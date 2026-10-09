//! Attribute names `trg` owns.
//!
//! Everything here is hand-written, unlike [`super::generated`], because it
//! names concepts specific to `trg` rather than an OTel semantic convention.
//! Kept in its own module so the two are never confused for one another.

/// Resource attribute: the subcommand path this process is running
/// (`mcp proxy`, `ai skills eval run`, ...). One `trg` process runs exactly
/// one command, so this is a resource fact, not a span attribute.
pub const COMMAND: &str = "trg.command";

/// The eval run's own identifier, attached to its `run` span.
pub const EVAL_RUN_ID: &str = "trg.eval.run.id";

/// The case under test.
pub const EVAL_CASE_ID: &str = "trg.eval.case.id";

/// Which split (train/test/...) the case belongs to.
pub const EVAL_CASE_SPLIT: &str = "trg.eval.case.split";

/// The scenario within the case.
pub const EVAL_SCENARIO: &str = "trg.eval.scenario";

/// The iteration number for a case/scenario pair.
pub const EVAL_ITERATION: &str = "trg.eval.iteration";

/// The tool grant the run executed under.
pub const EVAL_TOOL_GRANT: &str = "trg.eval.tool_grant";

/// Which `-j` lane a run executed on.
pub const EVAL_LANE: &str = "trg.eval.lane";

/// Whether a run's cache lookup hit.
pub const EVAL_CACHE_HIT: &str = "trg.eval.cache.hit";

/// The attempt number within a run.
pub const EVAL_ATTEMPT: &str = "trg.eval.attempt";

/// Cost in USD attributed to an attempt.
pub const EVAL_COST_USD: &str = "trg.eval.cost.usd";

/// Harness-reported duration in milliseconds, kept beside wall time so
/// harness startup overhead is visible.
pub const EVAL_HARNESS_DURATION_MS: &str = "trg.eval.harness.duration_ms";

/// The grader's name, on a `grade` span's children.
pub const EVAL_GRADER_NAME: &str = "trg.eval.grader.name";
