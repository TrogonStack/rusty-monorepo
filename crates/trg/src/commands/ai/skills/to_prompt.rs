use std::path::PathBuf;

use crate::agentskills::parser::{find_skill_md, read_properties};
use crate::agentskills::prompt::{to_prompt_with_location, SkillWithLocation};
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;
use serde_json::json;

use super::resolve_skill_path;

#[derive(Args)]
pub struct ToPromptArgs {
    #[arg(help = "Paths to skill directories or SKILL.md files")]
    pub paths: Vec<PathBuf>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the prompt on its own or wrapped in a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl ToPromptArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        let mut skills = Vec::new();
        let mut failed: Vec<String> = Vec::new();

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
                    failed.push(path.display().to_string());
                }
            }
        }

        if !failed.is_empty() && skills.is_empty() {
            return 1;
        }

        let prompt = to_prompt_with_location(&skills);

        // Asking for three skills and getting a prompt built from two is not a
        // success. The prompt is still worth printing, since the skills that
        // were read are usable, but a caller redirecting stdout to a file has
        // nothing else to go on. One code for both formats, because which
        // format was asked for does not change what happened.
        let code = i32::from(!failed.is_empty());

        if self.output_format.is_json() {
            // The paths that failed are named in the document as well as on
            // stderr. Otherwise a prompt built from three skills and a prompt
            // built from two of them plus a failure are the same document, and a
            // caller reading only stdout cannot tell which it got.
            let document = json!({
                "prompt": prompt,
                "skills": skills
                    .iter()
                    .map(|skill| json!({
                        "name": skill.properties.name,
                        "location": skill.location,
                    }))
                    .collect::<Vec<_>>(),
                "failed": failed,
            });
            return print_json(&document, code);
        }

        println!("{prompt}");
        code
    }
}

/// A prompt that is missing a skill the caller asked for.
///
/// The command still prints the skills it could read, so the only thing
/// separating a complete prompt from a short one is the exit code. It has to
/// say so under either format, since `--output-format text` is what a shell
/// script redirecting stdout to a file uses.
#[cfg(test)]
mod partial_reads {
    use super::*;
    use crate::fs::testutil::MemFS;

    fn skill(fs: &MemFS, name: &str) {
        fs.insert(
            format!("{name}/SKILL.md"),
            format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\n# {name}\n"),
        );
    }

    fn run(fs: &MemFS, paths: &[&str], output_format: OutputFormat) -> i32 {
        ToPromptArgs {
            paths: paths.iter().map(PathBuf::from).collect(),
            output_format,
        }
        .handle(fs)
    }

    #[test]
    fn every_skill_read_is_a_success() {
        let fs = MemFS::new();
        skill(&fs, "alpha");
        skill(&fs, "beta");

        assert_eq!(run(&fs, &["alpha", "beta"], OutputFormat::Text), 0);
        assert_eq!(run(&fs, &["alpha", "beta"], OutputFormat::Json), 0);
    }

    #[test]
    fn one_unreadable_skill_fails_under_either_format() {
        let fs = MemFS::new();
        skill(&fs, "alpha");

        assert_eq!(run(&fs, &["alpha", "missing"], OutputFormat::Text), 1);
        assert_eq!(run(&fs, &["alpha", "missing"], OutputFormat::Json), 1);
    }

    #[test]
    fn no_readable_skill_fails_under_either_format() {
        let fs = MemFS::new();

        assert_eq!(run(&fs, &["missing"], OutputFormat::Text), 1);
        assert_eq!(run(&fs, &["missing"], OutputFormat::Json), 1);
    }
}
