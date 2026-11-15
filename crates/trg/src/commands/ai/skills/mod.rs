mod eval;
mod read_properties;
mod to_prompt;
mod validate;

use std::path::{Path, PathBuf};

use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use clap::{ArgMatches, Args, FromArgMatches, Subcommand};

pub use eval::EvalArgs;
pub use read_properties::ReadPropertiesArgs;
pub use to_prompt::ToPromptArgs;
pub use validate::ValidateArgs;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum SkillsCommands {
    /// Validate a skill
    Validate(ValidateArgs),
    /// Run and verify skill evals
    Eval(EvalArgs),
    /// Read and print skill properties as JSON
    ReadProperties(ReadPropertiesArgs),
    /// Generate <available_skills> XML for agent prompts
    ToPrompt(ToPromptArgs),
}

impl SkillsCommands {
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        match self {
            Self::Validate(args) => args.handle(fs),
            Self::Eval(args) => args.handle(fs),
            Self::ReadProperties(args) => args.handle(fs),
            Self::ToPrompt(args) => args.handle(fs),
        }
    }
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct SkillLocation {
    #[arg(long, help = "Path to the skill directory")]
    skill_dir: Option<PathBuf>,

    #[arg(long, help = "Path to the SKILL.md file")]
    skill_file: Option<PathBuf>,
}

impl SkillLocation {
    pub fn resolve(&self) -> PathBuf {
        let path = self
            .skill_dir
            .as_deref()
            .or(self.skill_file.as_deref())
            .expect("clap requires one of --skill-dir and --skill-file");
        resolve_skill_path(path)
    }
}

#[derive(Args)]
#[group(required = true, multiple = true)]
struct SkillLocationFlags {
    #[arg(long, help = "Path to a skill directory; repeat for each skill")]
    skill_dir: Vec<PathBuf>,

    #[arg(long, help = "Path to a SKILL.md file; repeat for each skill")]
    skill_file: Vec<PathBuf>,
}

/// Skills named by `--skill-dir` and `--skill-file`, in command-line order.
pub struct SkillLocations {
    ordered: Vec<PathBuf>,
}

impl SkillLocations {
    pub fn new(ordered: Vec<PathBuf>) -> Self {
        Self { ordered }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        self.ordered.iter().map(PathBuf::as_path)
    }
}

impl FromArgMatches for SkillLocations {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let mut indexed: Vec<(usize, PathBuf)> = Vec::new();
        for id in ["skill_dir", "skill_file"] {
            if let (Some(indices), Some(values)) = (matches.indices_of(id), matches.get_many::<PathBuf>(id)) {
                indexed.extend(indices.zip(values.cloned()));
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        Ok(Self::new(indexed.into_iter().map(|(_, path)| path).collect()))
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Args for SkillLocations {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        SkillLocationFlags::augment_args(cmd)
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        SkillLocationFlags::augment_args_for_update(cmd)
    }

    fn group_id() -> Option<clap::Id> {
        SkillLocationFlags::group_id()
    }
}

pub fn resolve_skill_path(path: &Path) -> PathBuf {
    let is_skill_md = matches!(path.file_name().and_then(|n| n.to_str()), Some("SKILL.md" | "skill.md"));

    if is_skill_md {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests;
