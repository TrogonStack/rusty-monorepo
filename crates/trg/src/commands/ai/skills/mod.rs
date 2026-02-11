mod read_properties;
mod to_prompt;
mod validate;

use std::path::{Path, PathBuf};

use crate::fs::FileSystem;
use clap::Subcommand;

pub use read_properties::ReadPropertiesArgs;
pub use to_prompt::ToPromptArgs;
pub use validate::ValidateArgs;

#[derive(Subcommand)]
pub enum SkillsCommands {
    /// Validate a skill directory
    Validate(ValidateArgs),
    /// Read and print skill properties as JSON
    ReadProperties(ReadPropertiesArgs),
    /// Generate <available_skills> XML for agent prompts
    ToPrompt(ToPromptArgs),
}

impl SkillsCommands {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        match self {
            Self::Validate(args) => args.handle(fs),
            Self::ReadProperties(args) => args.handle(fs),
            Self::ToPrompt(args) => args.handle(fs),
        }
    }
}

pub fn resolve_skill_path(path: &Path) -> PathBuf {
    let is_skill_md = matches!(path.file_name().and_then(|n| n.to_str()), Some("SKILL.md" | "skill.md"));

    if is_skill_md {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_skill_path_directory() {
        let path = Path::new("/path/to/skill");
        let resolved = resolve_skill_path(path);
        assert_eq!(resolved, path.to_path_buf());
    }

    #[test]
    fn test_resolve_skill_path_uppercase_skill_md() {
        let path = Path::new("/path/to/skill/SKILL.md");
        let resolved = resolve_skill_path(path);
        assert_eq!(resolved, PathBuf::from("/path/to/skill"));
    }

    #[test]
    fn test_resolve_skill_path_lowercase_skill_md() {
        let path = Path::new("/path/to/skill/skill.md");
        let resolved = resolve_skill_path(path);
        assert_eq!(resolved, PathBuf::from("/path/to/skill"));
    }
}
