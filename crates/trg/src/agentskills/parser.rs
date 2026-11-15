use super::errors::{Result, SkillError};
use super::models::SkillProperties;
use crate::fs::FileSystem;
use gray_matter::engine::YAML;
use gray_matter::{Matter, Pod};
use std::path::Path;

pub fn find_skill_md(fs: &impl FileSystem, skill_path: &Path) -> Result<std::path::PathBuf> {
    let uppercase_path = skill_path.join("SKILL.md");
    let lowercase_path = skill_path.join("skill.md");

    if fs.exists(&uppercase_path) {
        Ok(uppercase_path)
    } else if fs.exists(&lowercase_path) {
        Ok(lowercase_path)
    } else {
        Err(SkillError::SkillFileNotFound)
    }
}

fn parse_frontmatter(content: &str) -> Result<Pod> {
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(content)?;
    parsed.data.ok_or(SkillError::MissingFrontmatter)
}

pub fn read_properties(fs: &impl FileSystem, skill_path: &Path) -> Result<(SkillProperties, Vec<String>)> {
    let skill_md = find_skill_md(fs, skill_path)?;
    let content = fs.read_to_string(&skill_md)?;
    let data = parse_frontmatter(&content)?;

    let keys: Vec<String> = data.as_hashmap()?.keys().cloned().collect();

    let props: SkillProperties = data.deserialize()?;

    ensure_required_frontmatter_field(&props.name, "name")?;
    ensure_required_frontmatter_field(&props.description, "description")?;

    Ok((props, keys))
}

fn ensure_required_frontmatter_field(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        Err(SkillError::EmptyField(field))
    } else {
        Ok(())
    }
}

/// Frontmatter fields embedded in eval runner prompts (full body lives in the symlinked skill).
pub fn skill_summary_from_content(content: &str) -> Result<(String, String)> {
    let data = parse_frontmatter(content)?;
    let props: SkillProperties = data.deserialize()?;
    ensure_required_frontmatter_field(&props.name, "name")?;
    ensure_required_frontmatter_field(&props.description, "description")?;
    Ok((props.name, props.description))
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
