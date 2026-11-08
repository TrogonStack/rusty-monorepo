use super::*;
use crate::fs::testutil::MemFS;
use crate::fs::RealFS;
use std::path::PathBuf;

fn shipped_skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills")
}

/// A skill this crate ships is held to the rules this crate enforces.
///
/// A shipped skill that its own validator rejects is a record claiming something the
/// thing it describes does not satisfy, and nothing else in the build would say so:
/// `SKILL.md` is prose to the compiler.
#[test]
fn every_shipped_skill_passes_validation() {
    let mut checked = 0usize;
    let entries = std::fs::read_dir(shipped_skills_dir()).expect("crates/trg/skills must exist");

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        checked += 1;
        if let Err(error) = validate_skill(&RealFS, &path) {
            panic!("shipped skill {} does not validate: {error}", path.display());
        }
    }

    assert!(
        checked >= 1,
        "expected this crate to still ship a skill, found {checked}"
    );
}

#[test]
fn the_eval_authoring_skill_is_shipped() {
    let path = shipped_skills_dir().join("trg-eval-authoring");
    let props = validate_skill(&RealFS, &path).expect("trg-eval-authoring must validate");
    assert_eq!(props.name, "trg-eval-authoring");
}

#[test]
fn the_eval_hillclimb_skill_is_shipped() {
    let path = shipped_skills_dir().join("trg-eval-hillclimb");
    let props = validate_skill(&RealFS, &path).expect("trg-eval-hillclimb must validate");
    assert_eq!(props.name, "trg-eval-hillclimb");
}

#[test]
fn test_validate_name_valid() {
    let result = validate_name("my-skill", Path::new("/my-skill"));
    assert!(result.is_ok());
}

#[test]
fn test_validate_name_empty() {
    let result = validate_name("", Path::new("/skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_whitespace_only() {
    let result = validate_name("   ", Path::new("/skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_too_long() {
    let long_name = "a".repeat(65);
    let result = validate_name(&long_name, Path::new("/skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_at_max_length() {
    let name = "a".repeat(64);
    let path_str = format!("/{}", name);
    let result = validate_name(&name, Path::new(&path_str));
    assert!(result.is_ok());
}

#[test]
fn test_validate_name_uppercase_rejected() {
    let result = validate_name("My-Skill", Path::new("/My-Skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_mixed_case_rejected() {
    let result = validate_name("mySkill", Path::new("/mySkill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_invalid_chars() {
    let result = validate_name("test_skill", Path::new("/test_skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_leading_hyphen() {
    let result = validate_name("-test-skill", Path::new("/-test-skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_trailing_hyphen() {
    let result = validate_name("test-skill-", Path::new("/test-skill-"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_consecutive_hyphens() {
    let result = validate_name("test--skill", Path::new("/test--skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_name_nfkc_normalized_silently() {
    let name = "te\u{FB01}le";
    let path_str = format!("/{}", name);
    let result = validate_name(name, Path::new(&path_str));
    assert!(result.is_ok());
}

#[test]
fn test_validate_name_nfkc_cross_form_directory_match() {
    let result = validate_name("te\u{FB01}le", Path::new("/tefile"));
    assert!(result.is_ok());
}

#[test]
fn test_validate_name_unicode_lowercase_accepted() {
    let result = validate_name("caf\u{00E9}", Path::new("/caf\u{00E9}"));
    assert!(result.is_ok());
}

#[test]
fn test_validate_name_directory_mismatch() {
    let result = validate_name("my-skill", Path::new("/other-skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_description_valid() {
    let result = validate_description("This is a valid description");
    assert!(result.is_ok());
}

#[test]
fn test_validate_description_empty() {
    let result = validate_description("");
    assert!(result.is_err());
}

#[test]
fn test_validate_description_whitespace_only() {
    let result = validate_description("   ");
    assert!(result.is_err());
}

#[test]
fn test_validate_description_too_long() {
    let long_desc = "a".repeat(1025);
    let result = validate_description(&long_desc);
    assert!(result.is_err());
}

#[test]
fn test_validate_description_at_max_length() {
    let desc = "a".repeat(1024);
    let result = validate_description(&desc);
    assert!(result.is_ok());
}

#[test]
fn test_validate_compatibility_valid() {
    let result = validate_compatibility("v1.0 compatible");
    assert!(result.is_ok());
}

#[test]
fn test_validate_compatibility_too_long() {
    let long_compat = "a".repeat(501);
    let result = validate_compatibility(&long_compat);
    assert!(result.is_err());
}

#[test]
fn test_validate_compatibility_at_max_length() {
    let compat = "a".repeat(500);
    let result = validate_compatibility(&compat);
    assert!(result.is_ok());
}

#[test]
fn test_validate_skill_valid() {
    let fs = MemFS::new();
    let content = "---\nname: test-skill\ndescription: A valid test skill\n---";
    fs.insert(Path::new("/test-skill/SKILL.md"), content);

    let result = validate_skill(&fs, Path::new("/test-skill"));
    assert!(result.is_ok());
}

#[test]
fn test_validate_skill_requires_matching_directory() {
    let fs = MemFS::new();
    let content = "---\nname: different-name\ndescription: A valid test skill\n---";
    fs.insert(Path::new("/test-skill/SKILL.md"), content);

    let result = validate_skill(&fs, Path::new("/test-skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_skill_rejects_unknown_fields() {
    let fs = MemFS::new();
    let content = "---\nname: test-skill\ndescription: A valid skill\nunknown-field: value\n---";
    fs.insert(Path::new("/test-skill/SKILL.md"), content);

    let result = validate_skill(&fs, Path::new("/test-skill"));
    assert!(result.is_err());
}

#[test]
fn test_validate_skill_uppercase_name_rejected() {
    let fs = MemFS::new();
    let content = "---\nname: Test-Skill\ndescription: A valid skill\n---";
    fs.insert(Path::new("/Test-Skill/SKILL.md"), content);

    let result = validate_skill(&fs, Path::new("/Test-Skill"));
    assert!(result.is_err());
}
