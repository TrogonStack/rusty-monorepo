use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProperties {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(rename = "allowed-tools", skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
}

impl SkillProperties {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
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
            allowed_tools: Some("bash python".to_string()),
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
  "allowed-tools": "bash python",
  "metadata": {
    "key": "value"
  }
}"#
        );
    }
}
