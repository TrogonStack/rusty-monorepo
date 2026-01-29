use thiserror::Error;

#[derive(Error, Debug)]
pub enum SkillError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parsing error: {0}")]
    YamlError(#[from] serde_yaml::Error),

    #[error("Validation failed: {}", format_errors(.0))]
    Validation(Vec<String>),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Not found: {0}")]
    NotFound(String),
}

fn format_errors(errors: &[String]) -> String {
    errors.join("; ")
}

pub type Result<T> = std::result::Result<T, SkillError>;
