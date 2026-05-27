use std::path::PathBuf;

use crate::agentskills::evals::{
    check_eval_suite, write_eval_manifest_scaffold, EvalCheckOptions,
};
use crate::fs::FileSystem;
use clap::Args;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval init --skill-dir ./skills/my-skill

Optional eval-level metadata (schema_version >= 2 in evals/evals.json):
  tags                  Categorize eval cases (e.g. smoke, regression)
  priority              low | normal | high | critical
  timeout_secs          Per-eval runner timeout override (overrides --timeout-secs)
  expected_output_files Paths under outputs/ the runner should produce (warn if missing)
  grader_hints          Opaque JSON passed through to script graders

Timeout precedence: per-eval timeout_secs > --timeout-secs > runner default (no limit).
")]
pub struct InitArgs {
    #[arg(long, value_name = "DIR", help = "Path to a skill directory containing SKILL.md")]
    pub skill_dir: PathBuf,

    #[arg(long, help = "Overwrite an existing evals/evals.json")]
    pub force: bool,
}

impl InitArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let props = match crate::agentskills::validator::validate_skill(fs, &self.skill_dir) {
            Ok(props) => props,
            Err(error) => {
                eprintln!("Skill validation failed: {error}");
                return 1;
            }
        };

        let evals_path = self.skill_dir.join("evals").join("evals.json");
        if fs.exists(&evals_path) && !self.force {
            eprintln!(
                "Refusing to overwrite existing eval manifest at {}",
                evals_path.display()
            );
            return 1;
        }

        if let Err(error) = std::fs::create_dir_all(self.skill_dir.join("evals")) {
            eprintln!("Failed to create evals directory: {error}");
            return 1;
        }

        if let Err(error) = write_eval_manifest_scaffold(fs, &self.skill_dir, &props.name) {
            eprintln!("Failed to write eval manifest: {error}");
            return 1;
        }

        if let Err(error) = check_eval_suite(
            fs,
            &self.skill_dir,
            &props.name,
            EvalCheckOptions {
                require_assertions: true,
                ..EvalCheckOptions::default()
            },
        ) {
            eprintln!("Generated eval manifest failed validation: {error}");
            return 1;
        }

        println!("Created {}", evals_path.display());
        0
    }
}

#[cfg(test)]
mod tests {
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
            force: false,
        }
        .handle(&crate::fs::RealFS);
        assert_eq!(status, 0);

        let suite = load_eval_suite(&crate::fs::RealFS, &skill_dir).unwrap();
        assert_eq!(suite.skill_name.as_str(), "demo-skill");
        assert_eq!(suite.evals.len(), 2);
        assert_eq!(suite.evals[0].id.as_str(), "example");
        assert!(!suite.evals[0].assertions.is_empty());
        assert_eq!(suite.evals[1].id.as_str(), "metadata-example");
        assert_eq!(suite.schema_version, 2);
    }

    #[test]
    fn init_command_refuses_to_overwrite_without_force() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(temp.path(), "demo-skill");
        std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
        std::fs::write(skill_dir.join("evals/evals.json"), "{}").unwrap();

        let status = InitArgs {
            skill_dir,
            force: false,
        }
        .handle(&crate::fs::RealFS);
        assert_eq!(status, 1);
    }

    #[test]
    fn init_command_scaffold_passes_verify_strict() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(temp.path(), "demo-skill");

        let init_status = InitArgs {
            skill_dir: skill_dir.clone(),
            force: false,
        }
        .handle(&crate::fs::RealFS);
        assert_eq!(init_status, 0);

        let verify_status = VerifyArgs {
            workspace: None,
            skill_dir: Some(skill_dir),
            mode: VerifyMode::Strict,
            require_assertions: false,
            json: false,
            ci: Default::default(),
        }
        .handle(&crate::fs::RealFS);
        assert_eq!(verify_status, 0);
    }
}
