use std::path::PathBuf;

use crate::fs::FileSystem;
use clap::Args;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ReadPropertiesArgs {
    #[arg(help = "Path to skill directory or SKILL.md file")]
    pub path: PathBuf,
}

impl ReadPropertiesArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let skill_path = resolve_skill_path(&self.path);
        match crate::agentskills::parser::read_properties(fs, &skill_path) {
            Ok((props, _)) => match props.to_json() {
                Ok(json) => {
                    println!("{}", json);
                    0
                }
                Err(e) => {
                    eprintln!("✗ Failed to serialize properties: {}", e);
                    1
                }
            },
            Err(e) => {
                eprintln!("✗ Failed to read properties: {}", e);
                1
            }
        }
    }
}
