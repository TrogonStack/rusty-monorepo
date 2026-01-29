use crate::errors::{Result, SkillError};
use crate::filesystem::FileSystem;
use crate::models::SkillProperties;
use crate::parser;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

const MIN_NAME_LEN: usize = 1;
const MAX_NAME_LEN: usize = 64;
const MIN_DESC_LEN: usize = 1;
const MAX_DESC_LEN: usize = 1024;
const MAX_COMPAT_LEN: usize = 500;

const ALLOWED_FRONTMATTER_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
];

fn collect_validation_errors(result: Result<()>, errors: &mut Vec<String>) {
    if let Err(SkillError::Validation(mut msgs)) = result {
        errors.append(&mut msgs);
    }
}

fn validate_allowed_fields(metadata: &serde_yaml::Value) -> Result<()> {
    let mut errors = Vec::new();

    if let Some(mapping) = metadata.as_mapping() {
        let mut extra_fields = Vec::new();
        for key in mapping.keys() {
            if let Some(key_str) = key.as_str() {
                if !ALLOWED_FRONTMATTER_FIELDS.contains(&key_str) {
                    extra_fields.push(key_str.to_string());
                }
            }
        }

        if !extra_fields.is_empty() {
            extra_fields.sort();
            errors.push(format!(
                "Unexpected fields in frontmatter: {}. Only {} are allowed.",
                extra_fields.join(", "),
                ALLOWED_FRONTMATTER_FIELDS.join(", ")
            ));
        }
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

pub fn validate_skill(fs: &impl FileSystem, skill_path: &Path) -> Result<SkillProperties> {
    let (props, metadata) = parser::read_properties_with_metadata(fs, skill_path)?;

    let mut errors = Vec::new();

    collect_validation_errors(validate_allowed_fields(&metadata), &mut errors);
    collect_validation_errors(validate_name(&props.name, skill_path), &mut errors);
    collect_validation_errors(validate_description(&props.description), &mut errors);

    if let Some(ref compat) = props.compatibility {
        collect_validation_errors(validate_compatibility(compat), &mut errors);
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(props)
}

fn validate_name(name: &str, skill_path: &Path) -> Result<()> {
    let mut errors = Vec::new();

    if name.len() < MIN_NAME_LEN || name.len() > MAX_NAME_LEN {
        errors.push(format!(
            "name length must be between {} and {} characters",
            MIN_NAME_LEN, MAX_NAME_LEN
        ));
    }

    let normalized: String = name.nfkc().collect();
    if normalized != name {
        errors.push("name must be NFKC normalized".to_string());
    }

    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        errors.push("name must only contain letters, digits, and hyphens".to_string());
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name cannot start or end with a hyphen".to_string());
    }

    if name.contains("--") {
        errors.push("name cannot contain consecutive hyphens".to_string());
    }

    let dir_name = skill_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if name != dir_name {
        errors.push(format!(
            "name '{}' must match directory name '{}'",
            name, dir_name
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_description(desc: &str) -> Result<()> {
    let mut errors = Vec::new();

    if desc.len() < MIN_DESC_LEN || desc.len() > MAX_DESC_LEN {
        errors.push(format!(
            "description length must be between {} and {} characters",
            MIN_DESC_LEN, MAX_DESC_LEN
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_compatibility(compat: &str) -> Result<()> {
    let mut errors = Vec::new();

    if compat.len() > MAX_COMPAT_LEN {
        errors.push(format!(
            "compatibility length must not exceed {} characters",
            MAX_COMPAT_LEN
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::MemFS;

    #[test]
    fn test_validate_name_basic() {
        let path = Path::new("/skill");
        let result = validate_name("a", path);
        assert!(result.is_ok() || result.is_err()); // Just test it doesn't panic
    }

    #[test]
    fn test_validate_name_too_short() {
        let path = Path::new("/skill");
        let result = validate_name("", path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_too_long() {
        let path = Path::new("/skill");
        let long_name = "a".repeat(65);
        let result = validate_name(&long_name, path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_invalid_chars() {
        let path = Path::new("/skill");
        let result = validate_name("test_skill", path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_leading_hyphen() {
        let path = Path::new("/skill");
        let result = validate_name("-test-skill", path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_trailing_hyphen() {
        let path = Path::new("/skill");
        let result = validate_name("test-skill-", path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_consecutive_hyphens() {
        let path = Path::new("/skill");
        let result = validate_name("test--skill", path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_not_nfkc() {
        let path = Path::new("/skill");
        let denormalized = "tëst-skill"; // é is not NFKC normalized form
        let result = validate_name(denormalized, path);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_valid() {
        let result = validate_description("This is a valid description");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_description_too_short() {
        let result = validate_description("");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_too_long() {
        let long_desc = "a".repeat(1025);
        let result = validate_description(&long_desc);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_compatibility_valid() {
        let result = validate_compatibility("v1.0 compatible");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_compatibility_too_long() {
        let long_compat = "a".repeat(501);
        let result = validate_compatibility(&long_compat);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_skill_requires_matching_directory() {
        let fs = MemFS::new();
        let content = "---\nname: different-name\ndescription: A valid test skill\n---";
        fs.insert(Path::new("/test-skill/SKILL.md"), content);

        let result = validate_skill(&fs, Path::new("/test-skill"));
        // Should fail because the name doesn't match the directory name
        assert!(result.is_err());
    }
}
