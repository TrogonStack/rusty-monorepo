use std::path::PathBuf;

use crate::agentskills::models::SkillProperties;
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ReadPropertiesArgs {
    #[arg(help = "Path to skill directory or SKILL.md file")]
    pub path: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the properties as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl ReadPropertiesArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let skill_path = resolve_skill_path(&self.path);
        let props = match crate::agentskills::parser::read_properties(fs, &skill_path) {
            Ok((props, _)) => props,
            Err(e) => {
                eprintln!("✗ Failed to read properties: {}", e);
                return 1;
            }
        };

        if self.output_format.is_json() {
            return print_json(&props, 0);
        }

        print_properties(&props);
        0
    }
}

fn print_properties(props: &SkillProperties) {
    println!("Name:        {}", props.name);
    println!("Description: {}", props.description);
    if let Some(compatibility) = &props.compatibility {
        println!("Compat:      {compatibility}");
    }
    if let Some(license) = &props.license {
        println!("License:     {license}");
    }
    if let Some(tools) = &props.allowed_tools {
        println!("Tools:       {}", tools.join(", "));
    }
    if let Some(metadata) = &props.metadata {
        let mut keys: Vec<&String> = metadata.keys().collect();
        keys.sort();
        for key in keys {
            println!("{key}: {}", metadata[key]);
        }
    }
}
