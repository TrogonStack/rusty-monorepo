use super::*;
use clap::Parser;

#[test]
fn test_resolve_skill_path_directory() {
    let path = Path::new("/path/to/skill");
    let resolved = resolve_skill_path(path);
    assert_eq!(resolved, path.to_path_buf());
}

#[test]
fn test_resolve_skill_path_uppercase_skill_md() {
    let path = Path::new("/path/to/skill/SKILL.md");
    let resolved = resolve_skill_path(path);
    assert_eq!(resolved, PathBuf::from("/path/to/skill"));
}

#[test]
fn test_resolve_skill_path_lowercase_skill_md() {
    let path = Path::new("/path/to/skill/skill.md");
    let resolved = resolve_skill_path(path);
    assert_eq!(resolved, PathBuf::from("/path/to/skill"));
}

#[derive(clap::Parser)]
struct Probe {
    #[command(flatten)]
    skill: SkillLocation,
}

fn parse(args: &[&str]) -> Result<Probe, clap::Error> {
    Probe::try_parse_from(std::iter::once("probe").chain(args.iter().copied()))
}

#[test]
fn a_skill_is_named_by_its_directory() {
    let probe = parse(&["--skill-dir", "/path/to/skill"]).expect("parse");
    assert_eq!(probe.skill.resolve(), PathBuf::from("/path/to/skill"));
}

#[test]
fn a_skill_is_named_by_its_file() {
    let probe = parse(&["--skill-file", "/path/to/skill/SKILL.md"]).expect("parse");
    assert_eq!(probe.skill.resolve(), PathBuf::from("/path/to/skill"));
}

#[derive(clap::Parser)]
struct ProbeMany {
    #[command(flatten)]
    skills: SkillLocations,
}

fn parse_many(args: &[&str]) -> Result<ProbeMany, clap::Error> {
    ProbeMany::try_parse_from(std::iter::once("probe").chain(args.iter().copied()))
}

#[test]
fn many_skills_keep_the_command_line_order_across_directory_and_file_flags() {
    let probe = parse_many(&["--skill-dir", "a", "--skill-file", "b/SKILL.md", "--skill-dir", "c"]).expect("parse");
    let named: Vec<&Path> = probe.skills.iter().collect();
    assert_eq!(named, [Path::new("a"), Path::new("b/SKILL.md"), Path::new("c")]);
}

#[test]
fn at_least_one_skill_is_required() {
    assert!(parse_many(&[]).is_err());
}

#[test]
fn a_skill_needs_exactly_one_of_directory_and_file() {
    assert!(parse(&[]).is_err());
    assert!(parse(&["--skill-dir", "a", "--skill-file", "b/SKILL.md"]).is_err());
}
