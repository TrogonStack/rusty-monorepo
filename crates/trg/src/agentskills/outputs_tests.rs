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
