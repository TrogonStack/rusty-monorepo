use super::*;
use crate::fs::RealFS;

fn write_skill(dir: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\nBody.\n"),
    )
    .expect("write SKILL.md");
}

fn under_test() -> Vec<StagedSkillDir> {
    vec![StagedSkillDir::companion("skill-under-test")]
}

#[test]
fn a_companion_skill_must_stay_inside_the_skill_directory_the_trust_gate_admitted() {
    assert!(CompanionSkill::parse("evals/companions/other").is_ok());
    assert!(CompanionSkill::parse("/etc/passwd").is_err());
    assert!(CompanionSkill::parse("../elsewhere").is_err());
    assert!(CompanionSkill::parse("   ").is_err());
}

#[test]
fn a_companion_resolves_to_the_shared_parent_the_unannounced_skill_uses() {
    let skill = tempfile::tempdir().expect("temp dir");
    write_skill(
        &skill.path().join("companions/other-skill"),
        "other-skill",
        "Does other work.",
    );

    let staged = resolve_companions(
        &RealFS,
        skill.path(),
        &[CompanionSkill::parse("companions/other-skill").expect("valid path")],
        &under_test(),
    )
    .expect("a valid companion resolves");

    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].directory().as_str(), "skills/other-skill/");
}

#[test]
fn a_companion_directory_that_is_not_a_skill_is_refused_by_name() {
    let skill = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(skill.path().join("companions/empty")).expect("create dir");

    let failure = resolve_companions(
        &RealFS,
        skill.path(),
        &[CompanionSkill::parse("companions/empty").expect("valid path")],
        &under_test(),
    )
    .expect_err("a directory with no SKILL.md is not a skill");

    assert!(
        failure.to_string().contains("companions/empty"),
        "the refusal has to name the companion: {failure}"
    );
}

#[test]
fn two_companions_that_stage_as_one_directory_are_refused_rather_than_one_overwriting_the_other() {
    let skill = tempfile::tempdir().expect("temp dir");
    write_skill(&skill.path().join("a/same-name"), "same-name", "One of two.");
    write_skill(&skill.path().join("b/same-name"), "same-name", "The other of two.");

    let failure = resolve_companions(
        &RealFS,
        skill.path(),
        &[
            CompanionSkill::parse("a/same-name").expect("valid path"),
            CompanionSkill::parse("b/same-name").expect("valid path"),
        ],
        &under_test(),
    )
    .expect_err("two companions cannot share a staged directory");

    assert!(matches!(failure, CompanionError::Collides { .. }), "{failure}");
}

#[test]
fn a_companion_that_would_land_on_the_skill_under_test_is_refused() {
    let skill = tempfile::tempdir().expect("temp dir");
    write_skill(
        &skill.path().join("companions/skill-under-test"),
        "skill-under-test",
        "Wears the name of the skill being measured.",
    );

    let failure = resolve_companions(
        &RealFS,
        skill.path(),
        &[CompanionSkill::parse("companions/skill-under-test").expect("valid path")],
        &under_test(),
    )
    .expect_err("a companion cannot replace the skill under test");

    assert!(
        matches!(failure, CompanionError::ShadowsSkillUnderTest { .. }),
        "{failure}"
    );
}

#[test]
fn a_case_that_declares_no_companion_has_no_digest_to_key_on() {
    let skill = tempfile::tempdir().expect("temp dir");

    assert_eq!(companion_digest(skill.path(), &[]).expect("no companions"), None);
}

#[test]
fn editing_a_companion_changes_the_digest_the_cache_key_is_built_from() {
    let skill = tempfile::tempdir().expect("temp dir");
    let companion = skill.path().join("companions/other-skill");
    write_skill(&companion, "other-skill", "Does other work.");
    let declared = [CompanionSkill::parse("companions/other-skill").expect("valid path")];

    let before = companion_digest(skill.path(), &declared).expect("digest");
    write_skill(&companion, "other-skill", "Does other work, described differently.");
    let after = companion_digest(skill.path(), &declared).expect("digest");

    assert_ne!(before, after, "a changed companion must not reuse a run made before it");
}
