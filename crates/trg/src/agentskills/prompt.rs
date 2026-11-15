mod constants;
pub use constants::{PROMPT_CONTRACT_VERSION, SKILL_DIR_UNANNOUNCED, SKILL_LINK_OLD, SKILL_LINK_WITH};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::errors::SkillError;
use super::evals::{EvalCase, SkillDisclosure};
use super::models::SkillProperties;
use super::outputs::EVAL_ARTIFACT_CONSTRAINTS;
use super::parser::skill_summary_from_content;
use super::report::ScenarioKind;

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

/// The name a skill goes by, which is the name a harness's own skill tool
/// reports when it invokes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SkillName(String);

impl SkillName {
    pub fn parse(value: &str) -> Result<Self, SkillError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SkillError::EmptyField("name"));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Does this token name this skill?
    ///
    /// The validator holds a name to lowercase, but the token is text a harness
    /// produced, so the reading does not rest on the harness having echoed the
    /// case back.
    pub fn named_by(&self, token: &str) -> bool {
        self.0.eq_ignore_ascii_case(token)
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct StagedSkillDir(String);

impl StagedSkillDir {
    /// Where this run's skill belongs, or `None` for the arm that has no skill.
    pub fn for_run(scenario: ScenarioKind, disclosure: SkillDisclosure, skill_name: &str) -> Option<Self> {
        let link = match (scenario, disclosure) {
            (ScenarioKind::WithoutSkill, _) => return None,
            (_, SkillDisclosure::Unannounced) => return Some(Self::companion(skill_name)),
            (ScenarioKind::WithSkill, SkillDisclosure::Announced) => SKILL_LINK_WITH.to_string(),
            (ScenarioKind::OldSkill, SkillDisclosure::Announced) => SKILL_LINK_OLD.to_string(),
        };
        Some(Self(link))
    }

    /// Where a skill staged only to be passed over goes.
    ///
    /// The same shared parent an unannounced skill uses, so a listing reports the skill
    /// under test and its distractors as peers and no prompt has to change to mention them.
    pub fn companion(skill_name: &str) -> Self {
        Self(format!("{SKILL_DIR_UNANNOUNCED}{}/", directory_segment(skill_name)))
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
#[path = "prompt_tests.rs"]
mod tests;
