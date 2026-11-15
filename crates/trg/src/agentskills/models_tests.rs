use super::*;

#[test]
fn test_skill_properties_serialization() {
    let props = SkillProperties {
        name: "test-skill".to_string(),
        description: "A test skill".to_string(),
        compatibility: None,
        license: None,
        allowed_tools: None,
        metadata: None,
    };

    let json = props.to_json().unwrap();
    assert_eq!(
        json,
        r#"{
  "name": "test-skill",
  "description": "A test skill"
}"#
    );
}

#[test]
fn test_skill_properties_with_optional_fields() {
    let mut metadata = HashMap::new();
    metadata.insert("key".to_string(), "value".to_string());

    let props = SkillProperties {
        name: "test-skill".to_string(),
        description: "A test skill".to_string(),
        compatibility: Some("v1.0".to_string()),
        license: Some("MIT".to_string()),
        allowed_tools: Some(vec!["bash".to_string(), "python".to_string()]),
        metadata: Some(metadata),
    };

    let json = props.to_json().unwrap();
    assert_eq!(
        json,
        r#"{
  "name": "test-skill",
  "description": "A test skill",
  "compatibility": "v1.0",
  "license": "MIT",
  "allowed-tools": [
    "bash",
    "python"
  ],
  "metadata": {
    "key": "value"
  }
}"#
    );
}
