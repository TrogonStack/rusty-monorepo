use crate::errors::{Result, SkillError};
use crate::filesystem::FileSystem;
use crate::models::SkillProperties;
use std::collections::HashMap;
use std::path::Path;

pub fn find_skill_md(fs: &impl FileSystem, skill_path: &Path) -> Result<std::path::PathBuf> {
    let uppercase_path = skill_path.join("SKILL.md");
    let lowercase_path = skill_path.join("skill.md");

    if fs.exists(&uppercase_path) {
        Ok(uppercase_path)
    } else if fs.exists(&lowercase_path) {
        Ok(lowercase_path)
    } else {
        Err(SkillError::NotFound(
            "No SKILL.md or skill.md found".to_string(),
        ))
    }
}

pub fn parse_frontmatter(content: &str) -> Result<(String, String)> {
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() || lines[0] != "---" {
        return Err(SkillError::Parse(
            "Frontmatter must start with ---".to_string(),
        ));
    }

    let end_idx = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| **line == "---")
        .map(|(i, _)| i)
        .ok_or_else(|| SkillError::Parse("Frontmatter must end with ---".to_string()))?;

    let frontmatter = lines[1..end_idx].join("\n");
    let body = if end_idx + 1 < lines.len() {
        lines[end_idx + 1..].join("\n")
    } else {
        String::new()
    };

    Ok((frontmatter, body))
}

pub fn read_properties(fs: &impl FileSystem, skill_path: &Path) -> Result<SkillProperties> {
    let (props, _) = read_properties_with_metadata(fs, skill_path)?;
    Ok(props)
}

pub fn read_properties_with_metadata(
    fs: &impl FileSystem,
    skill_path: &Path,
) -> Result<(SkillProperties, serde_yaml::Value)> {
    let skill_md = find_skill_md(fs, skill_path)?;
    let content = fs.read_to_string(&skill_md)?;
    let (frontmatter, _body) = parse_frontmatter(&content)?;

    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&frontmatter)?;

    let name = yaml_value
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SkillError::Parse("Missing 'name' field".to_string()))?
        .to_string();

    let description = yaml_value
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SkillError::Parse("Missing 'description' field".to_string()))?
        .to_string();

    let compatibility = yaml_value
        .get("compatibility")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let license = yaml_value
        .get("license")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let allowed_tools = yaml_value
        .get("allowed-tools")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let metadata = yaml_value
        .get("metadata")
        .and_then(|v| v.as_mapping())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    if let (serde_yaml::Value::String(ks), serde_yaml::Value::String(vs)) = (k, v) {
                        Some((ks.clone(), vs.clone()))
                    } else {
                        None
                    }
                })
                .collect::<HashMap<String, String>>()
        })
        .filter(|m| !m.is_empty());

    let props = SkillProperties {
        name,
        description,
        compatibility,
        license,
        allowed_tools,
        metadata,
    };

    Ok((props, yaml_value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::MemFS;
    use std::path::Path;

    #[test]
    fn test_find_skill_md_exists() {
        let fs = MemFS::new();
        fs.insert(Path::new("/skill/SKILL.md"), "---\nname: test\n---");

        let found = find_skill_md(&fs, Path::new("/skill")).unwrap();
        assert!(fs.exists(&found));
    }

    #[test]
    fn test_find_skill_md_uppercase_precedence() {
        let fs = MemFS::new();
        fs.insert(Path::new("/skill/SKILL.md"), "---\n---");
        fs.insert(Path::new("/skill/skill.md"), "---\n---");

        let found = find_skill_md(&fs, Path::new("/skill")).unwrap();
        assert_eq!(found, std::path::PathBuf::from("/skill/SKILL.md"));
    }

    #[test]
    fn test_find_skill_md_not_found() {
        let fs = MemFS::new();
        let result = find_skill_md(&fs, Path::new("/skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_frontmatter_valid() {
        let content = "---\nname: test\n---\nBody content";
        let (frontmatter, body) = parse_frontmatter(content).unwrap();

        assert_eq!(frontmatter, "name: test");
        assert_eq!(body, "Body content");
    }

    #[test]
    fn test_parse_frontmatter_no_body() {
        let content = "---\nname: test\n---";
        let (frontmatter, body) = parse_frontmatter(content).unwrap();

        assert_eq!(frontmatter, "name: test");
        assert!(body.is_empty());
    }

    #[test]
    fn test_parse_frontmatter_multiline() {
        let content = "---\nname: test\ndescription: |\n  Multi\n  line\n---";
        let (frontmatter, _body) = parse_frontmatter(content).unwrap();

        assert!(frontmatter.contains("name: test"));
    }

    #[test]
    fn test_parse_frontmatter_no_start() {
        let content = "name: test\n---";
        let result = parse_frontmatter(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_frontmatter_no_end() {
        let content = "---\nname: test";
        let result = parse_frontmatter(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_properties_basic() {
        let fs = MemFS::new();
        let content = "---\nname: test-skill\ndescription: Test Description\n---";
        fs.insert(Path::new("/skill/SKILL.md"), content);

        let props = read_properties(&fs, Path::new("/skill")).unwrap();
        assert_eq!(props.name, "test-skill");
        assert_eq!(props.description, "Test Description");
    }

    #[test]
    fn test_read_properties_with_optional_fields() {
        let fs = MemFS::new();
        let content = "---\nname: test-skill\ndescription: Test\nlicense: MIT\ncompatibility: v1.0\nallowed-tools: bash python\n---";
        fs.insert(Path::new("/skill/SKILL.md"), content);

        let props = read_properties(&fs, Path::new("/skill")).unwrap();
        assert_eq!(props.license, Some("MIT".to_string()));
        assert_eq!(props.compatibility, Some("v1.0".to_string()));
        assert_eq!(props.allowed_tools, Some("bash python".to_string()));
    }

    #[test]
    fn test_read_properties_file_not_found() {
        let fs = MemFS::new();
        let result = read_properties(&fs, Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn test_read_properties_missing_required_field() {
        let fs = MemFS::new();
        let content = "---\ndescription: Only has description, missing name\n---";
        fs.insert(Path::new("/skill/SKILL.md"), content);

        let result = read_properties(&fs, Path::new("/skill"));
        assert!(result.is_err());
    }

    #[test]
    fn test_read_properties_multiple_skills_isolated() {
        let fs = MemFS::new();
        fs.insert(
            Path::new("/skill1/SKILL.md"),
            "---\nname: skill1\ndescription: First\n---",
        );
        fs.insert(
            Path::new("/skill2/SKILL.md"),
            "---\nname: skill2\ndescription: Second\n---",
        );

        let props1 = read_properties(&fs, Path::new("/skill1")).unwrap();
        let props2 = read_properties(&fs, Path::new("/skill2")).unwrap();

        assert_eq!(props1.name, "skill1");
        assert_eq!(props2.name, "skill2");
    }
}
