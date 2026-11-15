mod constants;
use super::errors::{Result, SkillError};
use super::models::SkillProperties;
use super::parser;
use super::validation::{ValidationError, ValidationErrors};
use crate::fs::FileSystem;
use constants::{ALLOWED_FRONTMATTER_FIELDS, MAX_COMPAT_LEN, MAX_DESC_LEN, MAX_NAME_LEN};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

fn collect_validation_errors(result: Result<()>, errors: &mut ValidationErrors) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(SkillError::Validation(other)) => {
            errors.merge(other);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn validate_allowed_fields(keys: &[String]) -> Result<()> {
    let mut extra_fields: Vec<String> = keys
        .iter()
        .filter(|k| !ALLOWED_FRONTMATTER_FIELDS.contains(&k.as_str()))
        .cloned()
        .collect();

    if extra_fields.is_empty() {
        return Ok(());
    }

    extra_fields.sort();
    Err(SkillError::Validation(
        ValidationError::for_field(
            "frontmatter",
            format!(
                "Unexpected fields: {}. Only {} are allowed.",
                extra_fields.join(", "),
                ALLOWED_FRONTMATTER_FIELDS.join(", ")
            ),
        )
        .into(),
    ))
}

pub fn validate_skill(fs: &impl FileSystem, skill_path: &Path) -> Result<SkillProperties> {
    let (props, keys) = parser::read_properties(fs, skill_path)?;

    let mut errors = ValidationErrors::new();

    collect_validation_errors(validate_allowed_fields(&keys), &mut errors)?;
    collect_validation_errors(validate_name(&props.name, skill_path), &mut errors)?;
    collect_validation_errors(validate_description(&props.description), &mut errors)?;

    if let Some(ref compat) = props.compatibility {
        collect_validation_errors(validate_compatibility(compat), &mut errors)?;
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(props)
}

fn validate_name(name: &str, skill_path: &Path) -> Result<()> {
    let mut errors = ValidationErrors::new();

    if name.trim().is_empty() {
        errors.push(ValidationError::for_field("name", "must be a non-empty string"));
        return Err(SkillError::Validation(errors));
    }

    let normalized: String = name.trim().nfkc().collect();

    if normalized.chars().count() > MAX_NAME_LEN {
        errors.push(ValidationError::for_field(
            "name",
            format!("exceeds {} character limit", MAX_NAME_LEN),
        ));
    }

    if normalized != normalized.to_lowercase() {
        errors.push(ValidationError::for_field("name", "must be lowercase"));
    }

    if normalized.starts_with('-') || normalized.ends_with('-') {
        errors.push(ValidationError::for_field("name", "cannot start or end with a hyphen"));
    }

    if normalized.contains("--") {
        errors.push(ValidationError::for_field("name", "cannot contain consecutive hyphens"));
    }

    if !normalized.chars().all(|c| c.is_alphanumeric() || c == '-') {
        errors.push(ValidationError::for_field(
            "name",
            "contains invalid characters; only letters, digits, and hyphens are allowed",
        ));
    }

    let dir_name = skill_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let normalized_dir: String = dir_name.nfkc().collect();

    if normalized_dir != normalized {
        errors.push(ValidationError::for_field(
            "name",
            format!("'{}' must match directory name '{}'", name.trim(), dir_name),
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_description(desc: &str) -> Result<()> {
    let mut errors = ValidationErrors::new();

    if desc.trim().is_empty() {
        errors.push(ValidationError::for_field("description", "must be a non-empty string"));
        return Err(SkillError::Validation(errors));
    }

    if desc.chars().count() > MAX_DESC_LEN {
        errors.push(ValidationError::for_field(
            "description",
            format!("exceeds {} character limit", MAX_DESC_LEN),
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_compatibility(compat: &str) -> Result<()> {
    let mut errors = ValidationErrors::new();

    if compat.chars().count() > MAX_COMPAT_LEN {
        errors.push(ValidationError::for_field(
            "compatibility",
            format!("exceeds {} character limit", MAX_COMPAT_LEN),
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

#[cfg(test)]
#[path = "validator_tests.rs"]
mod tests;
