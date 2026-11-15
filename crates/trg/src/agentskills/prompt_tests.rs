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
        "graders": [{ "type": "contains", "text": "Summary mentions total revenue" }],
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
    let announced = StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Announced, "demo-skill").unwrap();
    assert_eq!(announced.as_str(), SKILL_LINK_WITH);

    let old = StagedSkillDir::for_run(ScenarioKind::OldSkill, SkillDisclosure::Announced, "demo-skill").unwrap();
    assert_eq!(old.as_str(), SKILL_LINK_OLD);

    let unannounced =
        StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Unannounced, "demo-skill").unwrap();
    assert_eq!(unannounced.as_str(), "skills/demo-skill/");
    assert!(!unannounced.as_str().starts_with('.'));

    assert!(StagedSkillDir::for_run(ScenarioKind::WithoutSkill, SkillDisclosure::Unannounced, "demo-skill").is_none());
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
