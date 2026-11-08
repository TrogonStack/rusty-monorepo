use super::super::*;
use crate::fs::testutil::MemFS;
use std::path::PathBuf;

fn skill(fs: &MemFS, name: &str) {
    fs.insert(
        format!("{name}/SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\n# {name}\n"),
    );
}

fn run(fs: &MemFS, paths: &[&str], output_format: OutputFormat) -> ExitCode {
    ToPromptArgs {
        skills: SkillLocations::new(paths.iter().map(PathBuf::from).collect()),
        output_format,
    }
    .handle(fs)
}

#[test]
fn every_skill_read_is_a_success() {
    let fs = MemFS::new();
    skill(&fs, "alpha");
    skill(&fs, "beta");

    assert_eq!(run(&fs, &["alpha", "beta"], OutputFormat::Text), ExitCode::Success);
    assert_eq!(run(&fs, &["alpha", "beta"], OutputFormat::Json), ExitCode::Success);
}

#[test]
fn one_unreadable_skill_fails_under_either_format() {
    let fs = MemFS::new();
    skill(&fs, "alpha");

    assert_eq!(run(&fs, &["alpha", "missing"], OutputFormat::Text), ExitCode::Failed);
    assert_eq!(run(&fs, &["alpha", "missing"], OutputFormat::Json), ExitCode::Failed);
}

#[test]
fn no_readable_skill_fails_under_either_format() {
    let fs = MemFS::new();

    assert_eq!(run(&fs, &["missing"], OutputFormat::Text), ExitCode::Failed);
    assert_eq!(run(&fs, &["missing"], OutputFormat::Json), ExitCode::Failed);
}
