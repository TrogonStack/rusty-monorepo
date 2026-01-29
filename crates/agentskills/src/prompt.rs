use crate::models::SkillProperties;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub struct SkillWithLocation {
    pub properties: SkillProperties,
    pub location: Option<String>,
}

pub fn to_prompt(skills: &[SkillProperties]) -> String {
    to_prompt_with_location(
        &skills
            .iter()
            .map(|p| SkillWithLocation {
                properties: p.clone(),
                location: None,
            })
            .collect::<Vec<_>>(),
    )
}

pub fn to_prompt_with_location(skills: &[SkillWithLocation]) -> String {
    let mut xml = String::from("<available_skills>\n");

    for skill in skills {
        xml.push_str("  <skill>\n");
        xml.push_str(&format!(
            "    <name>{}</name>\n",
            html_escape(&skill.properties.name)
        ));
        xml.push_str(&format!(
            "    <description>{}</description>\n",
            html_escape(&skill.properties.description)
        ));

        if let Some(ref compat) = skill.properties.compatibility {
            xml.push_str(&format!(
                "    <compatibility>{}</compatibility>\n",
                html_escape(compat)
            ));
        }

        if let Some(ref license) = skill.properties.license {
            xml.push_str(&format!(
                "    <license>{}</license>\n",
                html_escape(license)
            ));
        }

        if let Some(ref tools) = skill.properties.allowed_tools {
            xml.push_str(&format!(
                "    <allowed-tools>{}</allowed-tools>\n",
                html_escape(tools)
            ));
        }

        if let Some(ref location) = skill.location {
            xml.push_str(&format!(
                "    <location>{}</location>\n",
                html_escape(location)
            ));
        }

        xml.push_str("  </skill>\n");
    }

    xml.push_str("</available_skills>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("test & demo"), "test &amp; demo");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("quote\"test"), "quote&quot;test");
    }

    #[test]
    fn test_to_prompt_single_skill() {
        let skill = SkillProperties {
            name: "test-skill".to_string(),
            description: "Test Description".to_string(),
            compatibility: None,
            license: None,
            allowed_tools: None,
            metadata: None,
        };

        let xml = to_prompt(&[skill]);
        assert!(xml.contains("<skill>"));
        assert!(xml.contains("<name>test-skill</name>"));
        assert!(xml.contains("<description>Test Description</description>"));
        assert!(xml.contains("</skill>"));
    }

    #[test]
    fn test_to_prompt_multiple_skills() {
        let skills = vec![
            SkillProperties {
                name: "skill1".to_string(),
                description: "First skill".to_string(),
                compatibility: None,
                license: None,
                allowed_tools: None,
                metadata: None,
            },
            SkillProperties {
                name: "skill2".to_string(),
                description: "Second skill".to_string(),
                compatibility: None,
                license: None,
                allowed_tools: None,
                metadata: None,
            },
        ];

        let xml = to_prompt(&skills);
        assert!(xml.contains("<name>skill1</name>"));
        assert!(xml.contains("<name>skill2</name>"));
    }

    #[test]
    fn test_to_prompt_with_optional_fields() {
        let skill = SkillProperties {
            name: "test-skill".to_string(),
            description: "Test Description".to_string(),
            compatibility: Some("v1.0".to_string()),
            license: Some("MIT".to_string()),
            allowed_tools: Some("bash python".to_string()),
            metadata: None,
        };

        let xml = to_prompt(&[skill]);
        assert!(xml.contains("<compatibility>v1.0</compatibility>"));
        assert!(xml.contains("<license>MIT</license>"));
        assert!(xml.contains("<allowed-tools>bash python</allowed-tools>"));
    }

    #[test]
    fn test_to_prompt_html_escaping() {
        let skill = SkillProperties {
            name: "test<skill>".to_string(),
            description: "Test & Description".to_string(),
            compatibility: None,
            license: None,
            allowed_tools: None,
            metadata: None,
        };

        let xml = to_prompt(&[skill]);
        assert!(xml.contains("<name>test&lt;skill&gt;</name>"));
        assert!(xml.contains("<description>Test &amp; Description</description>"));
    }
}
