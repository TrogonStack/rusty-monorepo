use super::errors::{Result, SkillError};
use super::models::SkillProperties;
use crate::fs::FileSystem;
use gray_matter::{engine::YAML, Matter, Pod};
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

    if props.name.is_empty() {
        return Err(SkillError::EmptyField("name"));
    }
    if props.description.is_empty() {
        return Err(SkillError::EmptyField("description"));
    }

    Ok((props, keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::MemFS;
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
        let data = parse_frontmatter("---\nname: test\n---\nBody content").unwrap();
        let hash = data.as_hashmap().unwrap();
        assert_eq!(hash.get("name").unwrap().as_string().unwrap(), "test");
    }

    #[test]
    fn test_parse_frontmatter_no_body() {
        let data = parse_frontmatter("---\nname: test\n---").unwrap();
        let hash = data.as_hashmap().unwrap();
        assert_eq!(hash.get("name").unwrap().as_string().unwrap(), "test");
    }

    #[test]
    fn test_parse_frontmatter_multiline() {
        let data = parse_frontmatter("---\nname: test\ndescription: |\n  Multi\n  line\n---").unwrap();
        let hash = data.as_hashmap().unwrap();
        assert_eq!(hash.get("name").unwrap().as_string().unwrap(), "test");
        assert!(hash.get("description").unwrap().as_string().unwrap().contains("Multi"));
    }

    #[test]
    fn test_parse_frontmatter_no_start() {
        let result = parse_frontmatter("name: test\n---");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_frontmatter_no_end() {
        let result = parse_frontmatter("---\nname: test");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_properties_basic() {
        let fs = MemFS::new();
        let content = "---\nname: test-skill\ndescription: Test Description\n---";
        fs.insert(Path::new("/skill/SKILL.md"), content);

        let (props, _) = read_properties(&fs, Path::new("/skill")).unwrap();
        assert_eq!(props.name, "test-skill");
        assert_eq!(props.description, "Test Description");
    }

    #[test]
    fn test_read_properties_with_optional_fields() {
        let fs = MemFS::new();
        let content =
            "---\nname: test-skill\ndescription: Test\nlicense: MIT\ncompatibility: v1.0\nallowed-tools: bash python\n---";
        fs.insert(Path::new("/skill/SKILL.md"), content);

        let (props, _) = read_properties(&fs, Path::new("/skill")).unwrap();
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

        let (props1, _) = read_properties(&fs, Path::new("/skill1")).unwrap();
        let (props2, _) = read_properties(&fs, Path::new("/skill2")).unwrap();

        assert_eq!(props1.name, "skill1");
        assert_eq!(props2.name, "skill2");
    }
}
