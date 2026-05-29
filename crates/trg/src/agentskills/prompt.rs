use super::errors::SkillError;
use super::evals::EvalCase;
use super::models::SkillProperties;
use super::outputs::EVAL_ARTIFACT_CONSTRAINTS;
use super::parser::skill_summary_from_content;
use super::report::ScenarioKind;

pub const PROMPT_CONTRACT_VERSION: &str = "v1";
pub const SKILL_LINK_WITH: &str = ".skill/";
pub const SKILL_LINK_OLD: &str = ".old-skill/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptText(pub String);

impl PromptText {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

impl SkillSummary {
    pub fn from_skill_md(skill_md: &str) -> Result<Self, SkillError> {
        let (name, description) = skill_summary_from_content(skill_md)?;
        Ok(Self { name, description })
    }

    fn format_block(&self) -> String {
        format!(
            "Skill summary:\nname: {name}\ndescription: {description}",
            name = self.name,
            description = self.description,
        )
    }
}

pub struct EvalPromptInput<'a> {
    pub scenario: ScenarioKind,
    pub eval: &'a EvalCase,
    pub skill_md: Option<&'a str>,
}

pub fn build_eval_prompt(input: EvalPromptInput<'_>) -> Result<PromptText, SkillError> {
    let mut sections = vec![input.eval.prompt.as_str().to_string()];

    if !input.eval.files.is_empty() {
        let mut files = String::from("Input files:");
        for relative in &input.eval.files {
            files.push('\n');
            files.push_str("- ");
            files.push_str(relative.as_str());
        }
        sections.push(files);
    }

    match input.scenario {
        ScenarioKind::WithSkill => {
            let skill_md = input.skill_md.ok_or(SkillError::MissingFrontmatter)?;
            let summary = SkillSummary::from_skill_md(skill_md)?;
            sections.push(format!(
                "Skill available at: {SKILL_LINK_WITH}\n{}",
                summary.format_block()
            ));
        }
        ScenarioKind::WithoutSkill => {}
        ScenarioKind::OldSkill => {
            let skill_md = input.skill_md.ok_or(SkillError::MissingFrontmatter)?;
            let summary = SkillSummary::from_skill_md(skill_md)?;
            sections.push(format!(
                "Skill available at: {SKILL_LINK_OLD}\n{}",
                summary.format_block()
            ));
        }
    }

    sections.push(EVAL_ARTIFACT_CONSTRAINTS.to_string());
    Ok(PromptText(sections.join("\n\n")))
}

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
            allowed_tools: Some(vec!["bash".to_string(), "python".to_string()]),
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

    fn contract_fixture_case() -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "sales-summary",
            "prompt": "Analyze the staged sales file and write a summary.",
            "expected_output": "A markdown summary under outputs/.",
            "files": [
                "evals/files/sales.csv",
                "evals/files/readme.txt"
            ],
            "assertions": ["Summary mentions total revenue"],
        }))
        .unwrap()
    }

    const CONTRACT_SKILL_MD: &str = "---\nname: demo-skill\ndescription: Analyzes CSV sales data.\n---\n\n# Body\n\nFull instructions must not appear in the prompt.\n";

    const CONTRACT_OLD_SKILL_MD: &str = "---\nname: demo-skill\ndescription: Legacy CSV handler.\n---\n\n# Old body\n";

    #[test]
    fn eval_prompt_contract_version_is_v1() {
        assert_eq!(PROMPT_CONTRACT_VERSION, "v1");
    }

    #[test]
    fn eval_prompt_snapshot_with_skill() {
        let eval = contract_fixture_case();
        let prompt = build_eval_prompt(EvalPromptInput {
            scenario: ScenarioKind::WithSkill,
            eval: &eval,
            skill_md: Some(CONTRACT_SKILL_MD),
        })
        .unwrap();

        assert_eq!(
            prompt.as_str(),
            "\
Analyze the staged sales file and write a summary.

Input files:
- evals/files/sales.csv
- evals/files/readme.txt

Skill available at: .skill/
Skill summary:
name: demo-skill
description: Analyzes CSV sales data.

Write all deliverable files under outputs/. Do not write files outside outputs/."
        );
        assert!(!prompt.as_str().contains("Full instructions"));
    }

    #[test]
    fn eval_prompt_snapshot_without_skill_has_no_skill_mentions() {
        let eval = contract_fixture_case();
        let prompt = build_eval_prompt(EvalPromptInput {
            scenario: ScenarioKind::WithoutSkill,
            eval: &eval,
            skill_md: Some(CONTRACT_SKILL_MD),
        })
        .unwrap();

        assert_eq!(
            prompt.as_str(),
            "\
Analyze the staged sales file and write a summary.

Input files:
- evals/files/sales.csv
- evals/files/readme.txt

Write all deliverable files under outputs/. Do not write files outside outputs/."
        );
        assert!(!prompt.as_str().contains("Skill available at:"));
        assert!(!prompt.as_str().contains("Skill summary:"));
        assert!(!prompt.as_str().contains(".skill/"));
        assert!(!prompt.as_str().contains(".old-skill/"));
    }

    #[test]
    fn eval_prompt_snapshot_old_skill() {
        let eval = contract_fixture_case();
        let prompt = build_eval_prompt(EvalPromptInput {
            scenario: ScenarioKind::OldSkill,
            eval: &eval,
            skill_md: Some(CONTRACT_OLD_SKILL_MD),
        })
        .unwrap();

        assert_eq!(
            prompt.as_str(),
            "\
Analyze the staged sales file and write a summary.

Input files:
- evals/files/sales.csv
- evals/files/readme.txt

Skill available at: .old-skill/
Skill summary:
name: demo-skill
description: Legacy CSV handler.

Write all deliverable files under outputs/. Do not write files outside outputs/."
        );
        assert!(!prompt.as_str().contains("Analyzes CSV sales data"));
        assert!(!prompt.as_str().contains("Skill available at: .skill/"));
        assert!(!prompt.as_str().contains("# Old body"));
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
