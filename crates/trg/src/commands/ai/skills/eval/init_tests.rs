use super::*;
use crate::agentskills::evals::load_eval_suite;
use crate::commands::ai::skills::eval::verify::{VerifyArgs, VerifyMode};
use std::path::Path;

fn write_skill(root: &Path, name: &str) -> PathBuf {
    let skill_dir = root.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: demo\n---\n"),
    )
    .unwrap();
    skill_dir
}

#[test]
fn init_command_creates_eval_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "demo-skill");

    let status = InitArgs {
        skill_dir: skill_dir.clone(),
        eval_dir: None,
        force: false,
        output_format: crate::output::OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);
    assert_eq!(status, ExitCode::Success);

    let suite = load_eval_suite(&crate::fs::RealFS, &skill_dir, &EvalDirName::default()).unwrap();
    assert_eq!(suite.skill_name.as_str(), "demo-skill");
    assert_eq!(suite.evals.len(), 2);
    assert_eq!(suite.evals[0].id.as_str(), "produces-a-summary");
    assert!(!suite.evals[0].graders.is_empty());
    assert_eq!(suite.evals[1].id.as_str(), "triggers-the-skill");
}

#[test]
fn init_command_refuses_to_overwrite_without_force() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "demo-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(skill_dir.join("evals/evals.json"), "{}").unwrap();

    let status = InitArgs {
        skill_dir,
        eval_dir: None,
        force: false,
        output_format: crate::output::OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);
    assert_eq!(status, ExitCode::InfrastructureFailure);
}

#[test]
fn init_command_scaffold_passes_verify_strict() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "demo-skill");

    let init_status = InitArgs {
        skill_dir: skill_dir.clone(),
        eval_dir: None,
        force: false,
        output_format: crate::output::OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);
    assert_eq!(init_status, ExitCode::Success);

    let verify_status = VerifyArgs {
        workspace_dir: None,
        skill_dir: Some(skill_dir),
        eval_dir: None,
        mode: VerifyMode::Strict,
        require_graders: false,
        output_format: crate::output::OutputFormat::Text,
        ci: Default::default(),
    }
    .handle(&crate::fs::RealFS);
    assert_eq!(verify_status, ExitCode::Success);
}
