use super::super::prompt::{SKILL_DIR_UNANNOUNCED, SKILL_LINK_OLD, SKILL_LINK_WITH};
/// The `type` trogonai-agent gives the object that ends a `stream-json` run. Every line before it is an
/// event envelope, which carries no top-level `type` of its own.
pub const TROGONAI_AGENT_RESULT_TYPE: &str = "result";

/// The events that open and close one model call in opencode's `--format json` stream.
pub const OPENCODE_STEP_START_TYPE: &str = "step_start";
pub const OPENCODE_STEP_FINISH_TYPE: &str = "step_finish";

/// The event opencode streams when a run fails, such as a provider refusing its key.
pub const OPENCODE_ERROR_TYPE: &str = "error";

/// The event that closes a message in pi's `--mode json` stream; an assistant one ends a
/// model call and carries that call's usage.
pub const PI_MESSAGE_END_TYPE: &str = "message_end";

pub const NORMALIZED_TRANSCRIPT_FILE: &str = "events.json";

/// The directories a run's own skill is ever staged at.
pub const STAGED_SKILL_DIRS: &[&str] = &[SKILL_LINK_WITH, SKILL_LINK_OLD, SKILL_DIR_UNANNOUNCED];

/// The flags after which a command line carries another command line.
pub const INTERPRETER_FLAGS: &[&str] = &["-c", "-lc", "-ic", "-lic"];
