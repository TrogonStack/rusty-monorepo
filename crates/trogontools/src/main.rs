use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "trogontools")]
#[command(about = "TrogonStack tools and utilities")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Ai {
        #[command(subcommand)]
        command: AiCommands,
    },
}

#[derive(Subcommand)]
enum AiCommands {
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },
}

#[derive(Subcommand)]
enum SkillsCommands {
    Validate {
        #[arg(help = "Path to skill directory or SKILL.md file")]
        path: PathBuf,
    },
    ReadProperties {
        #[arg(help = "Path to skill directory or SKILL.md file")]
        path: PathBuf,
    },
    ToPrompt {
        #[arg(help = "Paths to skill directories or SKILL.md files")]
        paths: Vec<PathBuf>,
    },
}

fn resolve_skill_path(path: &Path) -> PathBuf {
    if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md")
        || path.file_name().and_then(|n| n.to_str()) == Some("skill.md")
    {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.to_path_buf()
    }
}

fn main() {
    let cli = Cli::parse();

    let exit_code = match cli.command {
        Commands::Ai { command } => match command {
            AiCommands::Skills { command } => match command {
                SkillsCommands::Validate { path } => {
                    let skill_path = resolve_skill_path(&path);
                    match agentskills::validate_skill(&skill_path) {
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
                SkillsCommands::ReadProperties { path } => {
                    let skill_path = resolve_skill_path(&path);
                    match agentskills::read_properties(&skill_path) {
                        Ok(props) => match props.to_json() {
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
                SkillsCommands::ToPrompt { paths } => {
                    let mut skill_items = Vec::new();
                    let mut had_error = false;

                    for path in paths {
                        let skill_path = resolve_skill_path(&path);
                        match (
                            agentskills::read_properties(&skill_path),
                            agentskills::parser::find_skill_md(&agentskills::RealFS, &skill_path),
                        ) {
                            (Ok(props), Ok(location)) => {
                                skill_items
                                    .push((props, Some(location.to_string_lossy().to_string())));
                            }
                            (Ok(props), Err(_)) => {
                                skill_items.push((props, None));
                            }
                            (Err(e), _) => {
                                eprintln!("✗ Failed to read skill from {:?}: {}", path, e);
                                had_error = true;
                            }
                        }
                    }

                    if had_error && skill_items.is_empty() {
                        std::process::exit(1);
                    }

                    let skills_with_loc: Vec<_> = skill_items
                        .iter()
                        .map(|(props, location)| agentskills::prompt::SkillWithLocation {
                            properties: props.clone(),
                            location: location.clone(),
                        })
                        .collect();

                    println!(
                        "{}",
                        agentskills::prompt::to_prompt_with_location(&skills_with_loc)
                    );
                    0
                }
            },
        },
    };

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
