use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::evals::RelativeSkillPath;

/// A script a case runs in its own workspace before any agent starts.
///
/// A skill whose work depends on the state of a directory cannot be asked about that work
/// in an empty one. A case that reaches for a lockfile, a repository mid-rebase, or a file
/// that is already wrong is asking a question the empty workspace does not contain, and
/// without a way to say so the case can only be written as if every skill were used on a
/// blank slate, which is not how any of them are used.
///
/// trg runs the script itself rather than asking a harness to, which is what keeps the
/// statement portable: the same case puts the same directory in front of every harness,
/// and none of them needs a setup mechanism of its own for it to work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceScaffold(RelativeSkillPath);

impl WorkspaceScaffold {
    pub fn script(&self) -> &RelativeSkillPath {
        &self.0
    }

    pub fn resolve_in(&self, skill_path: &Path) -> PathBuf {
        skill_path.join(self.0.as_path())
    }

    /// What the script says, as something a cache key can be built from.
    ///
    /// A run is only answerable for the workspace it was handed, so editing the script has
    /// to reach the key. Without this the second pass is served the first pass's artifacts
    /// and reports a state the run never saw.
    pub fn digest(&self, skill_path: &Path) -> Result<ScaffoldDigest, ScaffoldFailure> {
        let path = self.resolve_in(skill_path);
        let bytes = std::fs::read(&path).map_err(|source| self.unreadable(source))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Ok(ScaffoldDigest(format!(
            "sha256:{}",
            super::hex_encode(hasher.finalize())
        )))
    }

    fn unreadable(&self, source: std::io::Error) -> ScaffoldFailure {
        ScaffoldFailure::Unreadable {
            script: self.0.as_str().to_string(),
            source,
        }
    }

    fn run_in(&self, skill_path: &Path, workspace_dir: &Path) -> Result<(), ScaffoldFailure> {
        // The child changes into the workspace before it execs, so a skill directory given
        // as a relative path would be looked for inside the workspace and not found.
        let script = std::fs::canonicalize(self.resolve_in(skill_path)).map_err(|source| self.unreadable(source))?;
        let output = Command::new(&script)
            .current_dir(workspace_dir)
            .output()
            .map_err(|source| self.unreadable(source))?;

        if output.status.success() {
            return Ok(());
        }

        Err(ScaffoldFailure::Rejected {
            script: self.0.as_str().to_string(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScaffoldDigest(String);

impl ScaffoldDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether the operator agreed to run a case's scaffold as themselves.
///
/// The script is author-supplied code that runs with the operator's own reach, so nothing
/// runs it on the strength of a manifest alone. Withholding is the default because the
/// answer has to be given by whoever is accountable for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaffoldPermission {
    #[default]
    Withheld,
    Granted,
}

impl ScaffoldPermission {
    pub fn granted(allowed: bool) -> Self {
        if allowed {
            Self::Granted
        } else {
            Self::Withheld
        }
    }

    pub fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

#[derive(Debug, Error)]
pub enum ScaffoldFailure {
    #[error(
        "case declares a workspace scaffold '{script}', which runs author-supplied code as you; \
         pass --allow-scaffold to run it"
    )]
    Withheld { script: String },
    #[error("workspace scaffold '{script}' could not be run: {source}")]
    Unreadable {
        script: String,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace scaffold '{script}' failed{}{}", exit_detail(*exit_code), stderr_detail(stderr))]
    Rejected {
        script: String,
        exit_code: Option<i32>,
        stderr: String,
    },
}

fn exit_detail(exit_code: Option<i32>) -> String {
    exit_code
        .map(|code| format!(" with exit code {code}"))
        .unwrap_or_default()
}

fn stderr_detail(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// Bring a case's workspace to the state it asks about, before anything reads it.
///
/// A case that declared state and then ran without it would be scored against a workspace
/// it never asked for, which is worse than not running it at all: the score reads as an
/// answer about the skill when it is an answer about the wrong directory. So a withheld
/// permission fails this run and leaves the rest of the suite measurable, rather than
/// quietly proceeding on a blank slate.
pub fn scaffold_workspace(
    scaffold: Option<&WorkspaceScaffold>,
    permission: ScaffoldPermission,
    skill_path: &Path,
    workspace_dir: &Path,
) -> Result<(), ScaffoldFailure> {
    let Some(scaffold) = scaffold else {
        return Ok(());
    };

    if !permission.is_granted() {
        return Err(ScaffoldFailure::Withheld {
            script: scaffold.script().as_str().to_string(),
        });
    }

    scaffold.run_in(skill_path, workspace_dir)
}
