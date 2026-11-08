use super::*;

fn refuse_to_answer(_: &Path) -> Option<bool> {
    panic!("nothing should have been asked")
}

#[test]
fn a_skill_in_the_operators_own_tree_is_not_asked_about() {
    let tree = tempfile::tempdir().expect("temp dir");
    let skill = tree.path().join("skills/my-skill");
    std::fs::create_dir_all(&skill).expect("create skill dir");
    let mut store = TrustStore::default();

    let trust = admit_with(&skill, tree.path(), SkillTrust::Withheld, &mut store, refuse_to_answer);

    assert_eq!(trust, Ok(SkillTrust::Granted));
    assert!(!store.trusts(&skill), "an own tree is not a standing answer about it");
}

#[test]
fn a_foreign_skill_the_operator_vouched_for_on_the_command_line_is_not_asked_about() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let here = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    let trust = admit_with(
        foreign.path(),
        here.path(),
        SkillTrust::Granted,
        &mut store,
        refuse_to_answer,
    );

    assert_eq!(trust, Ok(SkillTrust::Granted));
}

#[test]
fn a_foreign_skill_is_asked_about_once_and_remembered() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let here = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    let first = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| {
        Some(true)
    });
    let second = admit_with(
        foreign.path(),
        here.path(),
        SkillTrust::Withheld,
        &mut store,
        refuse_to_answer,
    );

    assert_eq!(first, Ok(SkillTrust::Granted));
    assert_eq!(second, Ok(SkillTrust::Granted));
}

#[test]
fn a_foreign_skill_the_operator_said_no_to_does_not_run() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let here = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    let trust = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| {
        Some(false)
    });

    assert!(matches!(trust, Err(TrustError::Declined { .. })));
    assert!(!store.trusts(foreign.path()), "a no is not remembered as a yes");
}

#[test]
fn a_foreign_skill_nobody_can_be_asked_about_is_refused_rather_than_run() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let here = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    let trust = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| None);

    let Err(refusal) = trust else {
        panic!("a question nobody answered is not a yes");
    };
    assert!(
        refusal.to_string().contains("--trust-skill"),
        "the refusal has to say how to proceed: {refusal}"
    );
}

#[test]
fn a_remembered_answer_survives_the_file_it_is_written_to() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let home = tempfile::tempdir().expect("temp dir");
    let path = home.path().join("trg/trusted-skills.json");
    let mut store = TrustStore::default();
    store.record(foreign.path());
    store.save_to(&path).expect("save");

    assert!(TrustStore::load_from(&path).trusts(foreign.path()));
}

#[test]
fn the_filesystem_root_vouches_for_nothing_it_happens_to_contain() {
    let skill = tempfile::tempdir().expect("temp dir");

    assert!(WorkingTree::parse(Path::new("/")).is_err());
    assert_eq!(SkillOrigin::of(skill.path(), Path::new("/")), SkillOrigin::Foreign);
}

#[test]
fn a_pass_run_from_the_filesystem_root_is_still_asked_about() {
    let skill = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    let refusal = admit_with(skill.path(), Path::new("/"), SkillTrust::Withheld, &mut store, |_| None)
        .expect_err("nobody can be asked");

    assert!(matches!(refusal, TrustError::NothingToAsk { .. }));
}

#[test]
fn a_pass_nobody_had_to_answer_leaves_the_store_alone() {
    let foreign = tempfile::tempdir().expect("temp dir");
    let working = tempfile::tempdir().expect("temp dir");
    let mut store = TrustStore::default();

    admit_with(foreign.path(), working.path(), SkillTrust::Granted, &mut store, |_| {
        panic!("nobody should be asked")
    })
    .expect("the flag settles it");

    assert!(!store.has_an_answer_nobody_wrote_down());
}

#[test]
fn a_store_nobody_has_written_yet_trusts_nothing() {
    let home = tempfile::tempdir().expect("temp dir");

    let store = TrustStore::load_from(&home.path().join("trg/trusted-skills.json"));

    assert!(!store.trusts(home.path()));
}

#[test]
fn a_store_that_cannot_be_read_asks_again_rather_than_trusting_everything() {
    let home = tempfile::tempdir().expect("temp dir");
    let path = home.path().join("trusted-skills.json");
    std::fs::write(&path, "{ not json").expect("write");

    let store = TrustStore::load_from(&path);

    assert!(!store.trusts(home.path()));
}
