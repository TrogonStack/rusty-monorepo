use super::errors::{Result, SkillError};
use super::models::SkillProperties;
use super::parser;
use crate::fs::FileSystem;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

const MAX_NAME_LEN: usize = 64;
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

fn collect_validation_errors(result: Result<()>, errors: &mut Vec<String>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(SkillError::Validation(mut msgs)) => {
            errors.append(&mut msgs);
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
    Err(SkillError::Validation(vec![format!(
        "Unexpected fields in frontmatter: {}. Only {} are allowed.",
        extra_fields.join(", "),
        ALLOWED_FRONTMATTER_FIELDS.join(", ")
    )]))
}

pub fn validate_skill(fs: &impl FileSystem, skill_path: &Path) -> Result<SkillProperties> {
    let (props, keys) = parser::read_properties(fs, skill_path)?;

    let mut errors = Vec::new();

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
    let mut errors = Vec::new();

    if name.trim().is_empty() {
        errors.push("name must be a non-empty string".to_string());
        return Err(SkillError::Validation(errors));
    }

    let normalized: String = name.trim().nfkc().collect();

    if normalized.chars().count() > MAX_NAME_LEN {
        errors.push(format!("name exceeds {} character limit", MAX_NAME_LEN));
    }

    if normalized != normalized.to_lowercase() {
        errors.push("name must be lowercase".to_string());
    }

    if normalized.starts_with('-') || normalized.ends_with('-') {
        errors.push("name cannot start or end with a hyphen".to_string());
    }

    if normalized.contains("--") {
        errors.push("name cannot contain consecutive hyphens".to_string());
    }

    if !normalized.chars().all(|c| c.is_alphanumeric() || c == '-') {
        errors.push("name contains invalid characters; only letters, digits, and hyphens are allowed".to_string());
    }

    let dir_name = skill_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let normalized_dir: String = dir_name.nfkc().collect();

    if normalized_dir != normalized {
        errors.push(format!(
            "name '{}' must match directory name '{}'",
            name.trim(),
            dir_name
        ));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_description(desc: &str) -> Result<()> {
    let mut errors = Vec::new();

    if desc.trim().is_empty() {
        errors.push("description must be a non-empty string".to_string());
        return Err(SkillError::Validation(errors));
    }

    if desc.chars().count() > MAX_DESC_LEN {
        errors.push(format!("description exceeds {} character limit", MAX_DESC_LEN));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

fn validate_compatibility(compat: &str) -> Result<()> {
    let mut errors = Vec::new();

    if compat.chars().count() > MAX_COMPAT_LEN {
        errors.push(format!("compatibility exceeds {} character limit", MAX_COMPAT_LEN));
    }

    if !errors.is_empty() {
        return Err(SkillError::Validation(errors));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::MemFS;

    #[test]
    fn test_validate_name_valid() {
        let result = validate_name("my-skill", Path::new("/my-skill"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_name_empty() {
        let result = validate_name("", Path::new("/skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_whitespace_only() {
        let result = validate_name("   ", Path::new("/skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_too_long() {
        let long_name = "a".repeat(65);
        let result = validate_name(&long_name, Path::new("/skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_at_max_length() {
        let name = "a".repeat(64);
        let path_str = format!("/{}", name);
        let result = validate_name(&name, Path::new(&path_str));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_name_uppercase_rejected() {
        let result = validate_name("My-Skill", Path::new("/My-Skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_mixed_case_rejected() {
        let result = validate_name("mySkill", Path::new("/mySkill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_invalid_chars() {
        let result = validate_name("test_skill", Path::new("/test_skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_leading_hyphen() {
        let result = validate_name("-test-skill", Path::new("/-test-skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_trailing_hyphen() {
        let result = validate_name("test-skill-", Path::new("/test-skill-"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_consecutive_hyphens() {
        let result = validate_name("test--skill", Path::new("/test--skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_name_nfkc_normalized_silently() {
        let name = "te\u{FB01}le";
        let path_str = format!("/{}", name);
        let result = validate_name(name, Path::new(&path_str));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_name_nfkc_cross_form_directory_match() {
        let result = validate_name("te\u{FB01}le", Path::new("/tefile"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_name_unicode_lowercase_accepted() {
        let result = validate_name("caf\u{00E9}", Path::new("/caf\u{00E9}"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_name_directory_mismatch() {
        let result = validate_name("my-skill", Path::new("/other-skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_valid() {
        let result = validate_description("This is a valid description");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_description_empty() {
        let result = validate_description("");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_whitespace_only() {
        let result = validate_description("   ");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_too_long() {
        let long_desc = "a".repeat(1025);
        let result = validate_description(&long_desc);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_description_at_max_length() {
        let desc = "a".repeat(1024);
        let result = validate_description(&desc);
        assert!(result.is_ok());
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
    fn test_validate_compatibility_at_max_length() {
        let compat = "a".repeat(500);
        let result = validate_compatibility(&compat);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_skill_valid() {
        let fs = MemFS::new();
        let content = "---\nname: test-skill\ndescription: A valid test skill\n---";
        fs.insert(Path::new("/test-skill/SKILL.md"), content);

        let result = validate_skill(&fs, Path::new("/test-skill"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_skill_requires_matching_directory() {
        let fs = MemFS::new();
        let content = "---\nname: different-name\ndescription: A valid test skill\n---";
        fs.insert(Path::new("/test-skill/SKILL.md"), content);

        let result = validate_skill(&fs, Path::new("/test-skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_skill_rejects_unknown_fields() {
        let fs = MemFS::new();
        let content = "---\nname: test-skill\ndescription: A valid skill\nunknown-field: value\n---";
        fs.insert(Path::new("/test-skill/SKILL.md"), content);

        let result = validate_skill(&fs, Path::new("/test-skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_skill_uppercase_name_rejected() {
        let fs = MemFS::new();
        let content = "---\nname: Test-Skill\ndescription: A valid skill\n---";
        fs.insert(Path::new("/Test-Skill/SKILL.md"), content);

        let result = validate_skill(&fs, Path::new("/Test-Skill"));
        assert!(result.is_err());
    }
}
