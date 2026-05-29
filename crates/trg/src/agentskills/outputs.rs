//! Output artifact capture for skills eval runs.
//!
//! Each run workspace contains an `outputs/` directory where agent CLIs are
//! instructed to place deliverable files. After a run, files under `outputs/`
//! are indexed into `report.json` with path, size, SHA-256, and MIME type.
//!
//! **Fixture files:** Eval manifest fixtures copied into the workspace (e.g.
//! `evals/files/...`) are *not* included in the output artifact index — only
//! paths under `outputs/` are scanned.

use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const OUTPUTS_DIR: &str = "outputs";
pub const FINAL_MD: &str = "final.md";

pub const EVAL_ARTIFACT_CONSTRAINTS: &str =
    "Write all deliverable files under outputs/. Do not write files outside outputs/.";

pub const OUTPUTS_PROMPT_INSTRUCTION: &str = EVAL_ARTIFACT_CONSTRAINTS;

#[derive(Error, Debug)]
pub enum OutputError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("path '{path}' escapes workspace boundary '{base}'")]
    PathEscape { path: String, base: String },
}

pub type Result<T> = std::result::Result<T, OutputError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputArtifact {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

pub fn outputs_dir(workspace: &Path) -> PathBuf {
    workspace.join(OUTPUTS_DIR)
}

pub fn ensure_outputs_dir(workspace: &Path) -> io::Result<PathBuf> {
    let dir = outputs_dir(workspace);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn persist_final_markdown(workspace: &Path, text: &str) -> Result<PathBuf> {
    let dir = ensure_outputs_dir(workspace).map_err(OutputError::from)?;
    let path = dir.join(FINAL_MD);
    write_file_contained(&dir, &path, text.as_bytes())?;
    Ok(path)
}

pub fn copy_final_from_runner_temp(workspace: &Path, temp_path: &Path) -> Result<PathBuf> {
    let dir = ensure_outputs_dir(workspace).map_err(OutputError::from)?;
    let dest = dir.join(FINAL_MD);
    let bytes = std::fs::read(temp_path).map_err(OutputError::from)?;
    write_file_contained(&dir, &dest, &bytes)?;
    Ok(dest)
}

/// Walk `workspace/outputs/` and build artifact records with paths relative to `report_dir`.
pub fn index_output_artifacts(workspace_dir: &Path, report_dir: &Path) -> Result<Vec<OutputArtifact>> {
    let outputs_root = outputs_dir(workspace_dir);
    if !outputs_root.is_dir() {
        return Ok(Vec::new());
    }

    let canonical_workspace = canonicalize_existing(workspace_dir)?;
    let canonical_outputs = canonicalize_existing(&outputs_root)?;
    if !canonical_outputs.starts_with(&canonical_workspace) {
        return Err(OutputError::PathEscape {
            path: outputs_root.display().to_string(),
            base: workspace_dir.display().to_string(),
        });
    }

    let mut artifacts = Vec::new();
    walk_output_files(&canonical_outputs, &canonical_outputs, report_dir, &mut artifacts)?;
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(artifacts)
}

pub fn cleanup_runner_temp_files(workspace_dir: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(workspace_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".trg-codex-final.txt" || name.starts_with(".trg-") {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Returns the canonical path of `path` if it exists and lies under `base`.
pub fn path_within_base(base: &Path, path: &Path) -> Result<PathBuf> {
    let canonical_base = canonicalize_existing(base)?;
    let canonical_path = if path.exists() {
        canonicalize_existing(path)?
    } else {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let canonical_parent = canonicalize_existing(parent)?;
        let file_name = path.file_name().ok_or_else(|| OutputError::PathEscape {
            path: path.display().to_string(),
            base: base.display().to_string(),
        })?;
        canonical_parent.join(file_name)
    };

    if !canonical_path.starts_with(&canonical_base) {
        return Err(OutputError::PathEscape {
            path: path.display().to_string(),
            base: base.display().to_string(),
        });
    }
    Ok(canonical_path)
}

fn write_file_contained(base: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    path_within_base(base, path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(OutputError::from)?;
    }
    std::fs::write(path, bytes).map_err(OutputError::from)?;
    Ok(())
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            OutputError::PathEscape {
                path: path.display().to_string(),
                base: path.display().to_string(),
            }
        } else {
            OutputError::Io(e)
        }
    })
}

fn walk_output_files(
    outputs_root: &Path,
    current: &Path,
    report_dir: &Path,
    artifacts: &mut Vec<OutputArtifact>,
) -> Result<()> {
    for entry in std::fs::read_dir(current).map_err(OutputError::from)? {
        let entry = entry.map_err(OutputError::from)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(OutputError::from)?;

        if path_within_base(outputs_root, &path).is_err() {
            return Err(OutputError::PathEscape {
                path: path.display().to_string(),
                base: outputs_root.display().to_string(),
            });
        }

        if file_type.is_dir() {
            walk_output_files(outputs_root, &path, report_dir, artifacts)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(report_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.display().to_string());
            let metadata = std::fs::metadata(&path).map_err(OutputError::from)?;
            let bytes = std::fs::read(&path).map_err(OutputError::from)?;
            artifacts.push(OutputArtifact {
                path: relative,
                size_bytes: metadata.len(),
                sha256: file_sha256_digest(&bytes),
                mime_type: guess_mime_type(&path),
            });
        }
    }
    Ok(())
}

fn file_sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub(crate) fn guess_mime_type(path: &Path) -> Option<String> {
    match path.extension()?.to_str()? {
        "md" => Some("text/markdown".to_string()),
        "json" => Some("application/json".to_string()),
        "jsonl" => Some("application/x-ndjson".to_string()),
        "txt" => Some("text/plain".to_string()),
        "html" | "htm" => Some("text/html".to_string()),
        "csv" => Some("text/csv".to_string()),
        "xml" => Some("application/xml".to_string()),
        "yaml" | "yml" => Some("application/yaml".to_string()),
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "svg" => Some("image/svg+xml".to_string()),
        _ => None,
    }
}

/// Reject relative paths that traverse outside the workspace via `..` components.
pub fn validate_relative_output_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(OutputError::PathEscape {
                    path: relative.to_string(),
                    base: OUTPUTS_DIR.to_string(),
                });
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_sha256_digest_matches_known_vector() {
        assert_eq!(
            file_sha256_digest(b"hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn index_output_artifacts_records_size_hash_and_mime() {
        let temp = tempdir().unwrap();
        let report_dir = temp.path().join("report");
        let workspace = report_dir.join("runs/run-001/workspace");
        let outputs = workspace.join(OUTPUTS_DIR);
        std::fs::create_dir_all(&outputs).unwrap();
        std::fs::write(outputs.join("answer.md"), "# Answer\n").unwrap();
        std::fs::write(outputs.join("data.json"), r#"{"ok":true}"#).unwrap();

        let artifacts = index_output_artifacts(&workspace, &report_dir).unwrap();
        assert_eq!(artifacts.len(), 2);

        let md = artifacts.iter().find(|a| a.path.ends_with("answer.md")).unwrap();
        assert_eq!(md.size_bytes, 9);
        assert_eq!(md.mime_type.as_deref(), Some("text/markdown"));
        assert!(md.sha256.starts_with("sha256:"));

        let json = artifacts.iter().find(|a| a.path.ends_with("data.json")).unwrap();
        assert_eq!(json.mime_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn path_within_base_rejects_escape_via_parent_dir() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        let err = path_within_base(&workspace, &outside).unwrap_err();
        assert!(matches!(err, OutputError::PathEscape { .. }));
    }

    #[test]
    fn path_within_base_accepts_file_inside_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let outputs = workspace.join(OUTPUTS_DIR);
        std::fs::create_dir_all(&outputs).unwrap();
        let file = outputs.join("final.md");
        std::fs::write(&file, "done").unwrap();

        assert!(path_within_base(&outputs, &file).is_ok());
    }

    #[test]
    fn validate_relative_output_path_rejects_parent_segments() {
        let err = validate_relative_output_path("../escape.md").unwrap_err();
        assert!(matches!(err, OutputError::PathEscape { .. }));
    }

    #[test]
    fn persist_final_markdown_writes_under_outputs() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let path = persist_final_markdown(&workspace, "final answer").unwrap();
        assert!(path.ends_with("outputs/final.md"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "final answer");
    }

    #[test]
    fn cleanup_runner_temp_files_removes_dot_trg_files() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join(".trg-codex-final.txt"), "temp").unwrap();
        std::fs::write(workspace.join(".trg-scratch"), "x").unwrap();
        std::fs::write(workspace.join("keep.txt"), "stay").unwrap();

        cleanup_runner_temp_files(&workspace).unwrap();
        assert!(!workspace.join(".trg-codex-final.txt").exists());
        assert!(!workspace.join(".trg-scratch").exists());
        assert!(workspace.join("keep.txt").exists());
    }

    #[test]
    fn index_skips_missing_outputs_directory() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let artifacts = index_output_artifacts(&workspace, temp.path()).unwrap();
        assert!(artifacts.is_empty());
    }
}
