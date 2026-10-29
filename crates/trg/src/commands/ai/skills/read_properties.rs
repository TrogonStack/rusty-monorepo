use crate::agentskills::models::SkillProperties;
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;

use super::SkillLocation;

#[derive(Args)]
pub struct ReadPropertiesArgs {
    #[command(flatten)]
    pub skill: SkillLocation,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the properties as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl ReadPropertiesArgs {
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        let skill_path = self.skill.resolve();
        let props = match crate::agentskills::parser::read_properties(fs, &skill_path) {
            Ok((props, _)) => props,
            Err(e) => {
                eprintln!("✗ Failed to read properties: {}", e);
                return ExitCode::Failed;
            }
        };

        if self.output_format.is_json() {
            return print_json(&props, ExitCode::Success);
        }

        print_properties(&props);
        ExitCode::Success
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
