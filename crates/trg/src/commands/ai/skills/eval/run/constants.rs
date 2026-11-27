pub const GRADING_AUTO: &str = "auto";

pub const GRADING_NONE: &str = "none";

/// `error.type` for a run step that stopped the run, or a run that failed without saying
/// which way.
pub const STEP_FAILED: &str = "step_failed";

pub const RUN_FAILED: &str = "run_failed";

/// What a harness may read from a directory above its working directory: the instruction
/// files agents look for upward, and the skill folders some agents discover the same way.
pub const ANCESTOR_AGENT_CONTEXT: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "skills",
    ".claude/skills",
    ".agents/skills",
    ".codex/skills",
    ".opencode/skills",
    ".trogonai-agent/skills",
];

/// How many entries the warning names before it only counts the rest.
pub const ANCESTOR_AGENT_CONTEXT_NAMED: usize = 5;
