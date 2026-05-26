use super::validation::ValidationErrors;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SkillError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parsing error: {0}")]
    Yaml(#[from] gray_matter::Error),

    #[error("Validation failed: {0}")]
    Validation(ValidationErrors),

    #[error("No SKILL.md or skill.md found")]
    SkillFileNotFound,

    #[error("No valid frontmatter found")]
    MissingFrontmatter,

    #[error("Required field is empty: {0}")]
    EmptyField(&'static str),
}

pub type Result<T> = std::result::Result<T, SkillError>;
