pub mod errors;
pub mod filesystem;
pub mod models;
pub mod parser;
pub mod prompt;
pub mod validator;

pub use errors::{Result, SkillError};
pub use filesystem::{FileSystem, MemFS, RealFS};
pub use models::SkillProperties;
pub use prompt::{to_prompt, to_prompt_with_location, SkillWithLocation};

use std::path::Path;

// Public API with RealFS (most common usage)
pub fn read_properties(skill_path: &Path) -> Result<SkillProperties> {
    parser::read_properties(&RealFS, skill_path)
}

pub fn validate_skill(skill_path: &Path) -> Result<SkillProperties> {
    validator::validate_skill(&RealFS, skill_path)
}

// Re-export fs-specific versions for advanced usage
pub use parser::read_properties as read_properties_with_fs;
pub use validator::validate_skill as validate_skill_with_fs;
