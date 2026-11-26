use super::super::prompt::{SKILL_DIR_UNANNOUNCED, SKILL_LINK_OLD, SKILL_LINK_WITH};
/// The `type` fx gives the object that ends a `stream-json` run. Every line before it is an
/// event envelope, which carries no top-level `type` of its own.
pub const FX_RESULT_TYPE: &str = "result";

pub const NORMALIZED_TRANSCRIPT_FILE: &str = "events.json";

/// The directories a run's own skill is ever staged at.
pub const STAGED_SKILL_DIRS: &[&str] = &[SKILL_LINK_WITH, SKILL_LINK_OLD, SKILL_DIR_UNANNOUNCED];

/// The flags after which a command line carries another command line.
pub const INTERPRETER_FLAGS: &[&str] = &["-c", "-lc", "-ic", "-lic"];
