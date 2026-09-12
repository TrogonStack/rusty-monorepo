use super::errors::SkillError;
use super::evals::{EvalCase, SkillDisclosure};
use super::models::SkillProperties;
use super::outputs::EVAL_ARTIFACT_CONSTRAINTS;
use super::parser::skill_summary_from_content;
use super::report::ScenarioKind;

pub const PROMPT_CONTRACT_VERSION: &str = "v2";
pub const SKILL_LINK_WITH: &str = ".skill/";
pub const SKILL_LINK_OLD: &str = ".old-skill/";
/// Where an unannounced case stages the skill.
///
/// The announced links are dot-prefixed because the prompt points at them and
/// nothing else should have to see them. A prompt that says nothing can only be
/// answered from what the workspace shows, so an unannounced skill is staged
/// under a plain directory that a listing reports, named after the skill itself.
pub const SKILL_DIR_UNANNOUNCED: &str = "skills/";

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

/// The workspace-relative directory a run's skill is staged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedSkillDir(String);

impl StagedSkillDir {
    /// Where this run's skill belongs, or `None` for the arm that has no skill.
    pub fn for_run(scenario: ScenarioKind, disclosure: SkillDisclosure, skill_name: &str) -> Option<Self> {
        let link = match (scenario, disclosure) {
            (ScenarioKind::WithoutSkill, _) => return None,
            (_, SkillDisclosure::Unannounced) => {
                format!("{SKILL_DIR_UNANNOUNCED}{}/", directory_segment(skill_name))
            }
            (ScenarioKind::WithSkill, SkillDisclosure::Announced) => SKILL_LINK_WITH.to_string(),
            (ScenarioKind::OldSkill, SkillDisclosure::Announced) => SKILL_LINK_OLD.to_string(),
        };
        Some(Self(link))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A skill name is free text, and this has to be one directory inside the
/// workspace, so anything that is not a name character becomes a separator.
fn directory_segment(skill_name: &str) -> String {
    let mut segment = String::new();
    for ch in skill_name.chars() {
        let ch = if ch.is_ascii_alphanumeric() || ch == '_' {
            ch
        } else {
            '-'
        };
        if ch == '-' && (segment.is_empty() || segment.ends_with('-')) {
            continue;
        }
        segment.push(ch);
    }
    let trimmed = segment.trim_end_matches('-');
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed.to_string()
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

    if !matches!(input.scenario, ScenarioKind::WithoutSkill) {
        let skill_md = input.skill_md.ok_or(SkillError::MissingFrontmatter)?;
        let summary = SkillSummary::from_skill_md(skill_md)?;
        let disclosure = input.eval.skill_disclosure;
        let announced = StagedSkillDir::for_run(input.scenario, disclosure, &summary.name)
            .filter(|_| disclosure.announces_the_skill());
        if let Some(staged) = announced {
            sections.push(format!(
                "Skill available at: {}\n{}",
                staged.as_str(),
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
    fn eval_prompt_contract_version_is_v2() {
        assert_eq!(PROMPT_CONTRACT_VERSION, "v2");
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

    fn unannounced_contract_case() -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "triggers-the-skill",
            "prompt": "Turn the staged sales file into a summary for the finance team.",
            "expected_output": "The run reaches for the skill rather than improvising.",
            "skill_disclosure": "unannounced",
        }))
        .unwrap()
    }

    /// The point of an unannounced case is that the only difference between the
    /// arms is whether the skill is in the workspace, so the prompt cannot be a
    /// second difference.
    #[test]
    fn an_unannounced_case_gives_both_arms_the_same_prompt() {
        let eval = unannounced_contract_case();
        let with_skill = build_eval_prompt(EvalPromptInput {
            scenario: ScenarioKind::WithSkill,
            eval: &eval,
            skill_md: Some(CONTRACT_SKILL_MD),
        })
        .unwrap();
        let without_skill = build_eval_prompt(EvalPromptInput {
            scenario: ScenarioKind::WithoutSkill,
            eval: &eval,
            skill_md: Some(CONTRACT_SKILL_MD),
        })
        .unwrap();

        assert_eq!(with_skill.as_str(), without_skill.as_str());
        assert!(!with_skill.as_str().contains("Skill available at:"));
        assert!(!with_skill.as_str().contains("Skill summary:"));
        assert!(!with_skill.as_str().contains("demo-skill"));
        assert!(with_skill.as_str().contains("Turn the staged sales file"));
    }

    #[test]
    fn an_unannounced_skill_is_staged_where_a_listing_of_the_workspace_shows_it() {
        let announced =
            StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Announced, "demo-skill").unwrap();
        assert_eq!(announced.as_str(), SKILL_LINK_WITH);

        let old = StagedSkillDir::for_run(ScenarioKind::OldSkill, SkillDisclosure::Announced, "demo-skill").unwrap();
        assert_eq!(old.as_str(), SKILL_LINK_OLD);

        let unannounced =
            StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Unannounced, "demo-skill").unwrap();
        assert_eq!(unannounced.as_str(), "skills/demo-skill/");
        assert!(!unannounced.as_str().starts_with('.'));

        assert!(
            StagedSkillDir::for_run(ScenarioKind::WithoutSkill, SkillDisclosure::Unannounced, "demo-skill").is_none()
        );
    }

    #[test]
    fn a_skill_name_that_is_not_a_directory_name_still_stages_inside_the_workspace() {
        let staged =
            StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Unannounced, "Demo Skill/../x").unwrap();
        assert_eq!(staged.as_str(), "skills/Demo-Skill-x/");

        let unnamed = StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Unannounced, "///").unwrap();
        assert_eq!(unnamed.as_str(), "skills/skill/");
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
