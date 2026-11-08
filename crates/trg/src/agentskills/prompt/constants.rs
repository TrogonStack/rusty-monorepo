pub const PROMPT_CONTRACT_VERSION: &str = "v2";

pub const SKILL_LINK_WITH: &str = ".skill/";

pub const SKILL_LINK_OLD: &str = ".old-skill/";

/// Where an unannounced case stages the skill.
///
/// The announced links are dot-prefixed because the prompt points at them and
/// nothing else should have to see them. A prompt that says nothing can only be
/// answered from what the workspace shows, so an unannounced skill is staged
/// under a plain directory that a listing reports, named after the skill itself.
pub const SKILL_DIR_UNANNOUNCED: &str = "skills/";
