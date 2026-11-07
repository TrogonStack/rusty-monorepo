//! Skills staged beside the one under test, present only to be passed over.
//!
//! An unannounced case says nothing about the skill, so what it scores is a routing
//! decision: did the run reach for the skill on its own? A workspace holding exactly one
//! skill makes that decision for the run, because there is nothing else it could have
//! reached for and no way to tell a skill that won from a skill that was the only option.
//! The decision becomes the run's own once other skills are there to be passed over, which
//! is what a companion is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{de, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::evals::validate_relative_path;
use super::prompt::StagedSkillDir;
use super::validator::validate_skill;
use crate::fs::FileSystem;

/// A skill directory a case stages beside the one under test.
///
/// Declared relative to the skill directory, the way `files` and `scaffold` are. That is
/// also what settles the skill-trust question: `validate_relative_path` refuses an absolute
/// path and refuses one that climbs out, so a companion always resolves inside the skill
/// directory the trust gate already admitted. Staging one therefore executes against no
/// directory the operator has not already answered for, and the gate is satisfied by
/// construction rather than skipped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct CompanionSkill(String);

impl CompanionSkill {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_relative_path("companion skill", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    pub fn resolve_in(&self, skill_path: &Path) -> PathBuf {
        skill_path.join(self.as_path())
    }
}

impl std::fmt::Display for CompanionSkill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CompanionSkill {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum CompanionError {
    #[error("companion skill '{declared}' is not a skill directory: {source}")]
    NotASkill {
        declared: String,
        #[source]
        source: super::errors::SkillError,
    },
    #[error(
        "companion skills '{first}' and '{second}' both stage as '{directory}', so one would \
         overwrite the other and the run would be handed fewer skills to choose between than \
         the case declares"
    )]
    Collides {
        first: String,
        second: String,
        directory: String,
    },
    #[error(
        "companion skill '{declared}' stages as '{directory}', which is where the skill under \
         test is staged, so it would replace the skill the case is measuring"
    )]
    ShadowsSkillUnderTest { declared: String, directory: String },
}

/// One companion's source directory and the workspace directory it is staged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCompanion {
    source: PathBuf,
    directory: StagedSkillDir,
}

impl StagedCompanion {
    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn directory(&self) -> &StagedSkillDir {
        &self.directory
    }
}

/// Where each declared companion comes from and where it goes, or the first reason none of
/// them can be staged.
///
/// Every companion is put through `validate_skill` first. A directory that is not a skill
/// would stage as a folder of files the run cannot route to, which is not a distractor: the
/// case would read as measuring a choice while offering nothing to choose.
///
/// `reserved` holds the directories this case's own skill occupies, so a companion that
/// would land on one is refused rather than allowed to replace the thing being measured.
/// It is a list because the arm that stages an older revision stages it under that
/// revision's own name, which `--allow-skill-name-mismatch` lets differ from the current
/// one, and a companion wearing the older name would otherwise land on top of the very
/// revision that arm exists to measure.
pub fn resolve_companions(
    fs: &impl FileSystem,
    skill_path: &Path,
    companions: &[CompanionSkill],
    reserved: &[StagedSkillDir],
) -> Result<Vec<StagedCompanion>, CompanionError> {
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    let mut staged = Vec::with_capacity(companions.len());

    for companion in companions {
        let source = companion.resolve_in(skill_path);
        let props = validate_skill(fs, &source).map_err(|error| CompanionError::NotASkill {
            declared: companion.as_str().to_string(),
            source: error,
        })?;

        let directory = StagedSkillDir::companion(&props.name);
        if reserved.iter().any(|taken| taken.as_str() == directory.as_str()) {
            return Err(CompanionError::ShadowsSkillUnderTest {
                declared: companion.as_str().to_string(),
                directory: directory.as_str().to_string(),
            });
        }
        if let Some(first) = claimed.insert(directory.as_str().to_string(), companion.as_str().to_string()) {
            return Err(CompanionError::Collides {
                first,
                second: companion.as_str().to_string(),
                directory: directory.as_str().to_string(),
            });
        }

        staged.push(StagedCompanion { source, directory });
    }

    Ok(staged)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionDigest(String);

impl CompanionDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the companions hold, as something a cache key can be built from.
///
/// A companion is an input to the run the same way a fixture is, and nothing else in the
/// key notices it changing: `evals_hash` covers the bytes that declare the path, and
/// `skill_hash` is the digest of the skill under test. Without this, editing a distractor's
/// `description` until the run stops choosing it would be served the grade from before the
/// edit. `None` for a case that declares none, so an entry written before this field
/// existed keeps its key.
pub fn companion_digest(skill_path: &Path, companions: &[CompanionSkill]) -> std::io::Result<Option<CompanionDigest>> {
    if companions.is_empty() {
        return Ok(None);
    }

    let mut hasher = Sha256::new();
    for companion in companions {
        hasher.update(companion.as_str().as_bytes());
        hasher.update([0]);

        let root = companion.resolve_in(skill_path);
        let mut entries = Vec::new();
        collect_companion_entries(&root, Path::new(""), &mut entries)?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (relative, payload) in entries {
            hasher.update(relative.as_bytes());
            hasher.update([0]);
            hasher.update(payload.len().to_string().as_bytes());
            hasher.update([0]);
            hasher.update(&payload);
            hasher.update([0]);
        }
    }

    Ok(Some(CompanionDigest(format!(
        "sha256:{}",
        super::hex_encode(hasher.finalize())
    ))))
}

/// A link contributes the path it points at rather than the bytes behind it, because
/// following one can walk back into a directory already visited and never stop.
fn collect_companion_entries(
    root: &Path,
    relative_dir: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root.join(relative_dir))? {
        let entry = entry?;
        let relative = relative_dir.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            entries.push((
                slash_path(&relative),
                target.to_string_lossy().into_owned().into_bytes(),
            ));
        } else if file_type.is_dir() {
            collect_companion_entries(root, &relative, entries)?;
        } else if file_type.is_file() {
            entries.push((slash_path(&relative), std::fs::read(entry.path())?));
        }
    }
    Ok(())
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[path = "companion_skills_tests.rs"]
mod tests;
