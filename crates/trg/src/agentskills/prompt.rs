use super::models::SkillProperties;

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

#[cfg(test)]
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
    let mut lines = vec!["<available_skills>".to_string()];

    for skill in skills {
        lines.push("<skill>".to_string());
        lines.push("<name>".to_string());
        lines.push(html_escape(&skill.properties.name));
        lines.push("</name>".to_string());
        lines.push("<description>".to_string());
        lines.push(html_escape(&skill.properties.description));
        lines.push("</description>".to_string());

        if let Some(ref location) = skill.location {
            lines.push("<location>".to_string());
            lines.push(html_escape(location));
            lines.push("</location>".to_string());
        }

        lines.push("</skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("test & demo"), "test &amp; demo");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape(r#"quote"test"#), "quote&quot;test");
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
        assert_eq!(
            xml,
            "\
<available_skills>
<skill>
<name>
test-skill
</name>
<description>
Test Description
</description>
</skill>
</available_skills>"
        );
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
        assert_eq!(
            xml,
            "\
<available_skills>
<skill>
<name>
skill1
</name>
<description>
First skill
</description>
</skill>
<skill>
<name>
skill2
</name>
<description>
Second skill
</description>
</skill>
</available_skills>"
        );
    }

    #[test]
    fn test_to_prompt_with_location() {
        let skill = SkillWithLocation {
            properties: SkillProperties {
                name: "test-skill".to_string(),
                description: "Test Description".to_string(),
                compatibility: None,
                license: None,
                allowed_tools: None,
                metadata: None,
            },
            location: Some("/path/to/SKILL.md".to_string()),
        };

        let xml = to_prompt_with_location(&[skill]);
        assert_eq!(
            xml,
            "\
<available_skills>
<skill>
<name>
test-skill
</name>
<description>
Test Description
</description>
<location>
/path/to/SKILL.md
</location>
</skill>
</available_skills>"
        );
    }

    #[test]
    fn test_to_prompt_optional_fields_not_in_prompt() {
        let skill = SkillProperties {
            name: "test-skill".to_string(),
            description: "Test Description".to_string(),
            compatibility: Some("v1.0".to_string()),
            license: Some("MIT".to_string()),
            allowed_tools: Some("bash python".to_string()),
            metadata: None,
        };

        let xml = to_prompt(&[skill]);
        assert_eq!(
            xml,
            "\
<available_skills>
<skill>
<name>
test-skill
</name>
<description>
Test Description
</description>
</skill>
</available_skills>"
        );
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
        assert_eq!(
            xml,
            "\
<available_skills>
<skill>
<name>
test&lt;skill&gt;
</name>
<description>
Test &amp; Description
</description>
</skill>
</available_skills>"
        );
    }
}
