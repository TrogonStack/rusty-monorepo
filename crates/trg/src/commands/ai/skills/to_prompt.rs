use std::path::PathBuf;

use crate::agentskills::parser::{find_skill_md, read_properties};
use crate::agentskills::prompt::{to_prompt_with_location, SkillWithLocation};
use crate::fs::FileSystem;
use clap::Args;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ToPromptArgs {
    #[arg(help = "Paths to skill directories or SKILL.md files")]
    pub paths: Vec<PathBuf>,
}

impl ToPromptArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let mut skills = Vec::new();
        let mut had_error = false;

        for path in &self.paths {
            let skill_path = resolve_skill_path(path);
            match read_properties(fs, &skill_path) {
                Ok((props, _)) => {
                    let location = find_skill_md(fs, &skill_path)
                        .ok()
                        .map(|p| p.to_string_lossy().to_string());
                    skills.push(SkillWithLocation {
                        properties: props,
                        location,
                    });
                }
                Err(e) => {
                    eprintln!("✗ Failed to read skill from {:?}: {}", path, e);
                    had_error = true;
                }
            }
        }

        if had_error && skills.is_empty() {
            return 1;
        }

        println!("{}", to_prompt_with_location(&skills));
        0
    }
}
