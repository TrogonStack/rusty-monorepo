use std::path::PathBuf;

use crate::fs::FileSystem;
use clap::Args;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ValidateArgs {
    #[arg(help = "Path to skill directory or SKILL.md file")]
    pub path: PathBuf,
}

impl ValidateArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let skill_path = resolve_skill_path(&self.path);
        match crate::agentskills::validator::validate_skill(fs, &skill_path) {
            Ok(_) => {
                println!("✓ Skill is valid");
                0
            }
            Err(e) => {
                eprintln!("✗ Validation failed: {}", e);
                1
            }
        }
    }
}
