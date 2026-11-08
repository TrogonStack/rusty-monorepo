use crate::agentskills::parser::{find_skill_md, read_properties};
use crate::agentskills::prompt::{to_prompt_with_location, SkillWithLocation};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::{print_json, OutputFormat};
use clap::Args;
use serde::Serialize;

use super::{resolve_skill_path, SkillLocations};

#[derive(Args)]
pub struct ToPromptArgs {
    #[command(flatten)]
    pub skills: SkillLocations,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the prompt on its own or wrapped in a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl ToPromptArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        let mut skills = Vec::new();
        let mut failed: Vec<String> = Vec::new();

        for path in self.skills.iter() {
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
            return ExitCode::Failed;
        }

        let prompt = to_prompt_with_location(&skills);

        // Asking for three skills and getting a prompt built from two is not a
        // success. The prompt is still worth printing, since the skills that
        // were read are usable, but a caller redirecting stdout to a file has
        // nothing else to go on. One code for both formats, because which
        // format was asked for does not change what happened.
        let code = ExitCode::from_success(failed.is_empty());

        if self.output_format.is_json() {
            // The paths that failed are named in the document as well as on
            // stderr. Otherwise a prompt built from three skills and a prompt
            // built from two of them plus a failure are the same document, and a
            // caller reading only stdout cannot tell which it got.
            #[derive(Serialize)]
            struct PromptSkillDocument<'a> {
                name: &'a str,
                location: &'a Option<String>,
            }
            #[derive(Serialize)]
            struct PromptDocument<'a> {
                prompt: &'a str,
                skills: Vec<PromptSkillDocument<'a>>,
                failed: &'a [String],
            }
            let document = PromptDocument {
                prompt: &prompt,
                skills: skills
                    .iter()
                    .map(|skill| PromptSkillDocument {
                        name: &skill.properties.name,
                        location: &skill.location,
                    })
                    .collect(),
                failed: &failed,
            };
            return print_json(&document, code);
        }

        println!("{prompt}");
        code
    }
}

#[cfg(test)]
mod tests;
