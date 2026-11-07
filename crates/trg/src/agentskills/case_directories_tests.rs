use super::*;
use crate::fs::testutil::MemFS;

fn skill_md(fs: &MemFS, skill_dir: &str, name: &str) {
    fs.insert(
        Path::new(skill_dir).join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A demo skill.\n---\n\n# Body\n"),
    );
}

#[test]
fn a_suite_compiled_from_case_directories_carries_the_prompt_file_as_its_prompt() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.\n");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "The thing is done."}"#,
    );

    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert_eq!(compiled.suite.evals.len(), 1);
    assert_eq!(compiled.suite.evals[0].id.as_str(), "one");
    assert_eq!(compiled.suite.evals[0].prompt.as_str(), "Do the thing.");
    assert_eq!(compiled.suite.evals[0].expected_output.as_str(), "The thing is done.");
    assert!(matches!(compiled.source, EvalSource::CaseDirectories { .. }));
}

#[test]
fn a_case_directory_missing_prompt_md_is_refused_by_name() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "The thing is done."}"#,
    );

    let error = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap_err();

    assert!(error.to_string().contains("evals/one"));
    assert!(error.to_string().contains("prompt.md"));
}

#[test]
fn a_case_json_that_redeclares_the_directory_derived_id_is_refused() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"id": "not-one", "expected_output": "done"}"#,
    );

    let error = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap_err();

    assert!(error.to_string().contains("'id'"));
}

#[test]
fn a_manifest_at_a_custom_eval_dir_resolves_with_it() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
        Path::new("/skill/cases/evals.json"),
        r#"{"skill_name": "demo-skill", "evals": [{"id": "one", "prompt": "p", "expected_output": "e"}]}"#,
    );

    let custom = EvalDirName::parse("cases").unwrap();
    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &custom).unwrap();

    assert_eq!(compiled.suite.evals[0].id.as_str(), "one");
}

#[test]
fn a_manifest_declaring_a_different_eval_dir_than_it_was_found_in_is_refused() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
            Path::new("/skill/cases/evals.json"),
            r#"{"skill_name": "demo-skill", "eval_dir": "evals", "evals": [{"id": "one", "prompt": "p", "expected_output": "e"}]}"#,
        );

    let custom = EvalDirName::parse("cases").unwrap();
    let error = resolve_eval_suite(&fs, Path::new("/skill"), &custom).unwrap_err();

    assert!(error.to_string().contains("declares eval_dir 'evals'"));
    assert!(error.to_string().contains("found by looking in 'cases'"));
}

#[test]
fn a_manifest_declaring_the_eval_dir_it_was_actually_found_in_is_accepted() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
            Path::new("/skill/cases/evals.json"),
            r#"{"skill_name": "demo-skill", "eval_dir": "cases", "evals": [{"id": "one", "prompt": "p", "expected_output": "e"}]}"#,
        );

    let custom = EvalDirName::parse("cases").unwrap();
    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &custom).unwrap();

    assert_eq!(compiled.suite.evals[0].id.as_str(), "one");
}

#[test]
fn a_grader_file_stem_becomes_its_default_name() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "done"}"#,
    );
    fs.insert(
        Path::new("/skill/evals/one/graders/mentions-total.json"),
        r#"{"type": "contains", "text": "total"}"#,
    );

    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    let grader = &compiled.suite.evals[0].graders[0];
    assert_eq!(grader.name.as_ref().map(|n| n.as_str()), Some("mentions-total"));
}

#[test]
fn mixing_a_manifest_with_case_directories_in_one_suite_is_refused() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
        Path::new("/skill/evals/evals.json"),
        r#"{"skill_name": "demo-skill", "evals": [{"id": "manifest-case", "prompt": "p", "expected_output": "e"}]}"#,
    );
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "done"}"#,
    );

    let error = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap_err();

    assert!(error.to_string().contains("evals.json"));
    assert!(error.to_string().contains("one"));
}

#[test]
fn a_directory_that_holds_only_unrelated_files_is_not_mistaken_for_a_case() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
        Path::new("/skill/evals/evals.json"),
        r#"{"skill_name": "demo-skill", "evals": [{"id": "manifest-case", "prompt": "p", "expected_output": "e"}]}"#,
    );
    fs.insert(Path::new("/skill/evals/files/sales.csv"), "a,b\n1,2\n");

    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert!(matches!(compiled.source, EvalSource::Manifest { .. }));
    assert_eq!(compiled.suite.evals[0].id.as_str(), "manifest-case");
}

/// Suite-level mcp mocks live at `evals/mocks/<server>/<tool>.md`, a directory that
/// holds neither `prompt.md`, `case.json`, nor `graders/`. Pinning that it is not
/// picked up as a case named "mocks": if it ever were, it would both fail to run (no
/// prompt) and trip the manifest/case-directories "pick one layout" refusal for every
/// manifest-authored suite that also declares suite-level mocks.
#[test]
fn a_bare_mocks_directory_is_not_discovered_as_a_case_named_mocks() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(
        Path::new("/skill/evals/evals.json"),
        r#"{"skill_name": "demo-skill", "evals": [{"id": "manifest-case", "prompt": "p", "expected_output": "e"}]}"#,
    );
    fs.insert(
        Path::new("/skill/evals/mocks/github/create_issue.md"),
        "---\ntype: fixed\n---\n{}\n",
    );

    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert!(matches!(compiled.source, EvalSource::Manifest { .. }));
    assert_eq!(compiled.suite.evals.len(), 1);
    assert_eq!(compiled.suite.evals[0].id.as_str(), "manifest-case");
}

#[test]
fn the_manifest_hash_stays_the_raw_file_bytes_digest() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    let manifest = r#"{"skill_name": "demo-skill", "evals": [{"id": "one", "prompt": "p", "expected_output": "e"}]}"#;
    fs.insert(Path::new("/skill/evals/evals.json"), manifest);

    let compiled = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    let mut hasher = Sha256::new();
    hasher.update(manifest.as_bytes());
    let expected = format!("sha256:{}", super::super::hex_encode(hasher.finalize()));
    assert_eq!(compiled.hash, expected);
}

#[test]
fn the_same_logical_case_hashes_differently_under_each_regime() {
    let manifest_fs = MemFS::new();
    skill_md(&manifest_fs, "/skill", "demo-skill");
    manifest_fs.insert(
            Path::new("/skill/evals/evals.json"),
            r#"{"skill_name": "demo-skill", "evals": [{"id": "one", "prompt": "Do the thing.", "expected_output": "The thing is done."}]}"#,
        );

    let directory_fs = MemFS::new();
    skill_md(&directory_fs, "/skill", "demo-skill");
    directory_fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    directory_fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "The thing is done."}"#,
    );

    let manifest_compiled = resolve_eval_suite(&manifest_fs, Path::new("/skill"), &EvalDirName::default()).unwrap();
    let directory_compiled = resolve_eval_suite(&directory_fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert_ne!(manifest_compiled.hash, directory_compiled.hash);
}

#[test]
fn the_directory_hash_is_stable_across_repeated_resolutions() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "done"}"#,
    );

    let first = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();
    let second = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert_eq!(first.hash, second.hash);
}

#[test]
fn the_directory_hash_changes_when_a_grader_file_changes() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");
    fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
    fs.insert(
        Path::new("/skill/evals/one/case.json"),
        r#"{"expected_output": "done"}"#,
    );
    fs.insert(
        Path::new("/skill/evals/one/graders/mentions-total.json"),
        r#"{"type": "contains", "text": "total"}"#,
    );
    let before = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    fs.insert(
        Path::new("/skill/evals/one/graders/mentions-total.json"),
        r#"{"type": "contains", "text": "grand total"}"#,
    );
    let after = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap();

    assert_ne!(before.hash, after.hash);
}

#[test]
fn a_skill_with_neither_a_manifest_nor_case_directories_fails_the_same_way_it_always_has() {
    let fs = MemFS::new();
    skill_md(&fs, "/skill", "demo-skill");

    let error = resolve_eval_suite(&fs, Path::new("/skill"), &EvalDirName::default()).unwrap_err();

    assert!(matches!(error, EvalError::Io(_)));
}

// MemFS has no symlinks (see `FileSystem::is_symlink` there), so a symlink can only
// be expressed against the real filesystem. Without the `is_symlink` check this
// does not error: `fixtures/loop` points back at `fixtures`, and the walk chases it
// until the OS's own symlink-resolution limit kicks in, at which point `is_dir` and
// `is_file` (which swallow their errors) both read as false and the entry is
// silently dropped, so `resolve_eval_suite` returns `Ok` with an incomplete digest
// instead of rejecting the suite.
#[cfg(unix)]
#[test]
fn a_symlink_inside_a_case_directory_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("skill");
    std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: A demo skill.\n---\n\n# Body\n",
    )
    .unwrap();
    std::fs::write(skill_dir.join("evals/one/prompt.md"), "Do the thing.\n").unwrap();
    std::fs::write(
        skill_dir.join("evals/one/case.json"),
        r#"{"expected_output": "The thing is done."}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(
        skill_dir.join("evals/one/fixtures"),
        skill_dir.join("evals/one/fixtures/loop"),
    )
    .unwrap();

    let error = resolve_eval_suite(&crate::fs::RealFS, &skill_dir, &EvalDirName::default()).unwrap_err();

    match error {
        EvalError::Validation(errors) => {
            let message = errors.to_string();
            assert!(message.contains("symlink"), "{message}");
            assert!(message.contains("fixtures/loop"), "{message}");
        }
        other => panic!("expected a validation error naming the symlink, got {other:?}"),
    }
}
