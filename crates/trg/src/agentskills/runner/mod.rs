pub mod claude_code;
pub mod codex;
pub mod cursor_agent;

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::evals::EvalCase;
use super::report::ScenarioKind;

#[derive(Debug, Copy, Clone, clap::ValueEnum)]
pub enum Runner {
    #[value(name = "cursor-agent")]
    CursorAgent,
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
}

impl Runner {
    pub fn invoke(self, request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
        match self {
            Self::CursorAgent => cursor_agent::run(request),
            Self::ClaudeCode => claude_code::run(request),
            Self::Codex => codex::run(request),
        }
    }
}

pub struct EvalRunRequest<'a> {
    pub eval: &'a EvalCase,
    pub scenario: ScenarioKind,
    pub skill_md: &'a str,
    pub skill_path: &'a Path,
    pub workspace_dir: &'a Path,
    pub transcript_path: &'a Path,
    pub runner_model: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub enum RunStatus {
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalRunOutcome {
    pub status: RunStatus,
    pub duration_ms: u64,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub final_text: String,
}

#[derive(Error, Debug)]
pub enum RunnerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("scenario '{0}' is not supported by this runner")]
    UnsupportedScenario(String),

    #[error("failed to spawn '{program}': {source}")]
    Spawn { program: String, source: std::io::Error },

    #[error("'{program}' exited without emitting a terminal result event")]
    MissingResult { program: String },

    #[error("'{program}' produced invalid output: {detail}")]
    InvalidOutput { program: String, detail: String },
}

#[derive(Debug)]
pub struct PreparedPrompt {
    pub prompt: String,
}

pub fn prepare_workspace(request: &EvalRunRequest) -> Result<PreparedPrompt, RunnerError> {
    std::fs::create_dir_all(request.workspace_dir)?;

    let prompt = match request.scenario {
        ScenarioKind::WithSkill => {
            symlink_skill_into_workspace(request.skill_path, request.workspace_dir)?;
            for relative in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
            }
            format!(
                "Use the skill at .skill/SKILL.md to handle the request below. The skill contents are:\n\n{skill_md}\n\n---\n\nRequest:\n{prompt}",
                skill_md = request.skill_md,
                prompt = request.eval.prompt,
            )
        }
        ScenarioKind::WithoutSkill => {
            for relative in &request.eval.files {
                stage_eval_file(request.skill_path, request.workspace_dir, relative.as_str())?;
            }
            request.eval.prompt.as_str().to_string()
        }
        ScenarioKind::OldSkill => {
            return Err(RunnerError::UnsupportedScenario(request.scenario.as_str().to_string()));
        }
    };

    Ok(PreparedPrompt { prompt })
}

#[cfg(unix)]
fn symlink_skill_into_workspace(skill_path: &Path, workspace_dir: &Path) -> std::io::Result<()> {
    let link = workspace_dir.join(".skill");
    if link.exists() || link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link).ok();
    }
    let absolute = std::fs::canonicalize(skill_path)?;
    std::os::unix::fs::symlink(absolute, link)
}

#[cfg(not(unix))]
fn symlink_skill_into_workspace(skill_path: &Path, workspace_dir: &Path) -> std::io::Result<()> {
    let dest = workspace_dir.join(".skill");
    copy_dir_recursive(skill_path, &dest)
}

fn stage_eval_file(skill_path: &Path, workspace_dir: &Path, relative: &str) -> std::io::Result<()> {
    let source = skill_path.join(relative);
    if !source.exists() {
        return Ok(());
    }

    let dest = workspace_dir.join(relative);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if source.is_dir() {
        copy_dir_recursive(&source, &dest)?;
    } else {
        std::fs::copy(&source, &dest)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub fn write_transcript(transcript_path: &Path, raw_stdout: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = transcript_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(transcript_path, raw_stdout)
}

pub fn write_timing_file(timing_path: &Path, outcome: &EvalRunOutcome) -> std::io::Result<()> {
    if let Some(parent) = timing_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let total = outcome.total_tokens.unwrap_or(0);
    let body = serde_json::json!({
        "total_tokens": total,
        "duration_ms": outcome.duration_ms,
    });
    std::fs::write(timing_path, serde_json::to_string_pretty(&body).unwrap())
}

pub type SkillDigest = BTreeMap<String, String>;

pub fn compute_skill_digest(skill_path: &Path) -> std::io::Result<SkillDigest> {
    let mut digest = BTreeMap::new();
    walk_and_hash(skill_path, skill_path, &mut digest)?;
    Ok(digest)
}

fn walk_and_hash(root: &Path, dir: &Path, digest: &mut SkillDigest) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_and_hash(root, &path, digest)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path)?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            digest.insert(relative, format!("sha256:{:x}", hasher.finalize()));
        }
    }
    Ok(())
}

pub fn detect_tampering(before: &SkillDigest, after: &SkillDigest) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (path, hash_before) in before {
        match after.get(path) {
            Some(hash_after) if hash_after == hash_before => {}
            Some(_) => changed.push(path.clone()),
            None => changed.push(path.clone()),
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::evals::EvalCase;
    use tempfile::tempdir;

    fn make_case(files: Vec<String>) -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "files": files,
            "assertions": [],
        }))
        .unwrap()
    }

    #[test]
    fn prepare_with_skill_symlinks_skill_and_prefixes_prompt() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
        std::fs::write(skill_path.join("SKILL.md"), "# Skill\n").unwrap();
        std::fs::write(skill_path.join("evals/files/input.txt"), "hello").unwrap();

        let workspace = temp.path().join("ws");
        let case = make_case(vec!["evals/files/input.txt".to_string()]);

        let request = EvalRunRequest {
            eval: &case,
            scenario: ScenarioKind::WithSkill,
            skill_md: "# Skill\n",
            skill_path: &skill_path,
            workspace_dir: &workspace,
            transcript_path: &workspace.join("transcript.jsonl"),
            runner_model: None,
        };

        let prepared = prepare_workspace(&request).unwrap();
        assert!(prepared.prompt.contains("Use the skill at .skill/SKILL.md"));
        assert!(prepared.prompt.contains("do the thing"));

        let link = workspace.join(".skill");
        #[cfg(unix)]
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(link.join("SKILL.md").is_file());
        assert!(workspace.join("evals/files/input.txt").is_file());
    }

    #[test]
    fn prepare_without_skill_returns_raw_prompt() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        std::fs::write(skill_path.join("SKILL.md"), "# Skill\n").unwrap();
        let workspace = temp.path().join("ws");

        let case = make_case(vec![]);
        let request = EvalRunRequest {
            eval: &case,
            scenario: ScenarioKind::WithoutSkill,
            skill_md: "# Skill\n",
            skill_path: &skill_path,
            workspace_dir: &workspace,
            transcript_path: &workspace.join("transcript.jsonl"),
            runner_model: None,
        };

        let prepared = prepare_workspace(&request).unwrap();
        assert_eq!(prepared.prompt, "do the thing");
        assert!(!workspace.join(".skill").exists());
    }

    #[test]
    fn prepare_old_skill_returns_unsupported() {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        let workspace = temp.path().join("ws");

        let case = make_case(vec![]);
        let request = EvalRunRequest {
            eval: &case,
            scenario: ScenarioKind::OldSkill,
            skill_md: "",
            skill_path: &skill_path,
            workspace_dir: &workspace,
            transcript_path: &workspace.join("transcript.jsonl"),
            runner_model: None,
        };

        let err = prepare_workspace(&request).unwrap_err();
        assert!(matches!(err, RunnerError::UnsupportedScenario(_)));
    }

    #[test]
    fn detect_tampering_flags_changes_additions_and_removals() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "original").unwrap();
        std::fs::write(skill.join("notes.md"), "keep").unwrap();

        let before = compute_skill_digest(&skill).unwrap();
        assert_eq!(before.len(), 2);

        std::fs::write(skill.join("SKILL.md"), "tampered").unwrap();
        std::fs::write(skill.join("added.md"), "new").unwrap();
        std::fs::remove_file(skill.join("notes.md")).unwrap();

        let after = compute_skill_digest(&skill).unwrap();
        let changed = detect_tampering(&before, &after);
        assert_eq!(changed, vec!["SKILL.md", "added.md", "notes.md"]);
    }

    #[test]
    fn detect_tampering_returns_empty_when_unchanged() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "stable").unwrap();
        let before = compute_skill_digest(&skill).unwrap();
        let after = compute_skill_digest(&skill).unwrap();
        assert!(detect_tampering(&before, &after).is_empty());
    }
}
