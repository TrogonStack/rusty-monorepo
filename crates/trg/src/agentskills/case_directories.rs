//! Resolving an eval suite from either its flat manifest or a directory-per-case
//! layout, so every caller runs and verifies the same definition.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::evals::{
    parse_eval_suite, EvalCase, EvalError, EvalSuite, Result, EVAL_SUITE_DIR_NAME, EVAL_SUITE_MANIFEST_NAME,
};
use super::validation::ValidationError;
use crate::fs::FileSystem;

const PROMPT_FILE_NAME: &str = "prompt.md";
const CASE_JSON_NAME: &str = "case.json";
const GRADERS_DIR_NAME: &str = "graders";
const DIRECTORY_HASH_REGIME_TAG: &str = "trg-eval-case-directories-v1";

/// Where a suite's cases were authored, and the bytes that decide its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalSource {
    Manifest { path: PathBuf },
    CaseDirectories { root: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CompiledSuite {
    pub suite: EvalSuite,
    pub source: EvalSource,
    pub hash: String,
}

/// Loads and compiles a skill's eval suite, from whichever layout it is authored in.
///
/// This is the only function that may decide where a suite's cases come from. Every
/// other reader (grading, running, reporting, drift detection) must call this instead
/// of joining `evals/evals.json` itself, or a directory-authored suite could run under
/// one definition and be verified under another.
pub fn resolve_eval_suite(fs: &impl FileSystem, skill_path: &Path) -> Result<CompiledSuite> {
    let evals_dir = skill_path.join(EVAL_SUITE_DIR_NAME);
    let manifest_path = evals_dir.join(EVAL_SUITE_MANIFEST_NAME);
    let manifest_present = fs.is_file(&manifest_path);
    let case_ids = discover_case_directories(fs, &evals_dir)?;

    if manifest_present && !case_ids.is_empty() {
        return Err(mixing_regimes_error(&manifest_path, &case_ids));
    }

    if !case_ids.is_empty() {
        let suite = compile_suite_from_directories(fs, skill_path, &evals_dir, &case_ids)?;
        let hash = canonical_directory_hash(fs, &evals_dir, &case_ids)?;
        return Ok(CompiledSuite {
            suite,
            source: EvalSource::CaseDirectories { root: evals_dir },
            hash,
        });
    }

    let content = fs.read_to_string(&manifest_path)?;
    let suite = parse_eval_suite(&content)?;
    let hash = format!("sha256:{}", super::hex_encode(Sha256::digest(content.as_bytes())));
    Ok(CompiledSuite {
        suite,
        source: EvalSource::Manifest { path: manifest_path },
        hash,
    })
}

fn mixing_regimes_error(manifest_path: &Path, case_ids: &[String]) -> EvalError {
    EvalError::Validation(
        ValidationError::for_field(
            "evals",
            format!(
                "found both a manifest at '{}' and case directories ({}); pick one layout for this suite",
                manifest_path.display(),
                case_ids.join(", ")
            ),
        )
        .into(),
    )
}

fn discover_case_directories(fs: &impl FileSystem, evals_dir: &Path) -> Result<Vec<String>> {
    if !fs.is_dir(evals_dir) {
        return Ok(Vec::new());
    }

    let mut ids = Vec::new();
    for entry in fs.read_dir(evals_dir)? {
        if !fs.is_dir(&entry) {
            continue;
        }
        let looks_like_a_case = fs.is_file(&entry.join(PROMPT_FILE_NAME))
            || fs.is_file(&entry.join(CASE_JSON_NAME))
            || fs.is_dir(&entry.join(GRADERS_DIR_NAME));
        if !looks_like_a_case {
            continue;
        }
        let id = entry
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                EvalError::Validation(
                    ValidationError::for_field(
                        "evals",
                        format!(
                            "case directory '{}' has a name that is not valid UTF-8",
                            entry.display()
                        ),
                    )
                    .into(),
                )
            })?
            .to_string();
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}

fn compile_suite_from_directories(
    fs: &impl FileSystem,
    skill_path: &Path,
    evals_dir: &Path,
    case_ids: &[String],
) -> Result<EvalSuite> {
    let skill_name = read_skill_name(fs, skill_path)?;

    let mut evals = Vec::with_capacity(case_ids.len());
    for case_id in case_ids {
        let case_dir = evals_dir.join(case_id);
        let case = compile_case_from_directory(fs, &case_dir, case_id)?;
        evals.push(serde_json::to_value(case).expect("a compiled EvalCase serializes infallibly"));
    }

    let suite_value = serde_json::json!({
        "skill_name": skill_name,
        "evals": evals,
    });
    serde_json::from_value::<EvalSuite>(suite_value).map_err(EvalError::from)
}

fn read_skill_name(fs: &impl FileSystem, skill_path: &Path) -> Result<String> {
    let (props, _keys) = super::parser::read_properties(fs, skill_path).map_err(|error| {
        EvalError::Validation(
            ValidationError::for_field(
                "skill_name",
                format!("could not derive skill_name from SKILL.md: {error}"),
            )
            .into(),
        )
    })?;
    Ok(props.name)
}

fn compile_case_from_directory(fs: &impl FileSystem, case_dir: &Path, case_id: &str) -> Result<EvalCase> {
    let prompt_path = case_dir.join(PROMPT_FILE_NAME);
    if !fs.is_file(&prompt_path) {
        return Err(EvalError::Validation(
            ValidationError::for_field(
                "prompt.md",
                format!("case directory '{}' is missing prompt.md", case_dir.display()),
            )
            .into(),
        ));
    }
    let prompt = fs.read_to_string(&prompt_path)?;

    let mut fields = serde_json::Map::new();
    let case_json_path = case_dir.join(CASE_JSON_NAME);
    if fs.is_file(&case_json_path) {
        let content = fs.read_to_string(&case_json_path)?;
        let parsed: serde_json::Value = serde_json::from_str(&content).map_err(EvalError::from)?;
        let object = parsed.as_object().cloned().ok_or_else(|| {
            EvalError::Validation(
                ValidationError::for_field(
                    "case.json",
                    format!("'{}' must contain a JSON object", case_json_path.display()),
                )
                .into(),
            )
        })?;
        for reserved in ["id", "prompt", "graders"] {
            if object.contains_key(reserved) {
                return Err(EvalError::Validation(
                    ValidationError::for_field(
                        "case.json",
                        format!(
                            "'{}' must not declare '{}'; it comes from the case directory",
                            case_json_path.display(),
                            reserved
                        ),
                    )
                    .into(),
                ));
            }
        }
        fields = object;
    }

    fields.insert("id".to_string(), serde_json::Value::String(case_id.to_string()));
    fields.insert(
        "prompt".to_string(),
        serde_json::Value::String(prompt.trim().to_string()),
    );

    let graders_dir = case_dir.join(GRADERS_DIR_NAME);
    if fs.is_dir(&graders_dir) {
        let mut grader_paths: Vec<PathBuf> = fs
            .read_dir(&graders_dir)?
            .into_iter()
            .filter(|path| fs.is_file(path) && path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect();
        grader_paths.sort();

        let mut graders = Vec::with_capacity(grader_paths.len());
        for grader_path in grader_paths {
            let content = fs.read_to_string(&grader_path)?;
            let mut grader_value: serde_json::Value = serde_json::from_str(&content).map_err(EvalError::from)?;
            if let Some(object) = grader_value.as_object_mut() {
                if !object.contains_key("name") {
                    let stem = grader_path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default()
                        .to_string();
                    object.insert("name".to_string(), serde_json::Value::String(stem));
                }
            }
            graders.push(grader_value);
        }
        fields.insert("graders".to_string(), serde_json::Value::Array(graders));
    }

    serde_json::from_value::<EvalCase>(serde_json::Value::Object(fields)).map_err(|error| {
        EvalError::Validation(
            ValidationError::for_field("evals", format!("case directory '{}': {}", case_dir.display(), error)).into(),
        )
    })
}

fn collect_case_files(fs: &impl FileSystem, case_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative_dir) = pending.pop() {
        let absolute_dir = case_dir.join(&relative_dir);
        for entry in fs.read_dir(&absolute_dir)? {
            let Some(name) = entry.file_name() else {
                continue;
            };
            let relative_entry = relative_dir.join(name);
            // Checked ahead of is_dir/is_file: a symlink to a directory reads as a
            // directory too, and following it can recurse without bound (a link back
            // to one of its own ancestors) or fold bytes the suite does not own into
            // the digest that is supposed to be a function of the suite's own bytes.
            if fs.is_symlink(&entry) {
                return Err(EvalError::Validation(
                    ValidationError::for_field(
                        "evals",
                        format!(
                            "eval case directories may not contain symlinks, but '{}' is one; \
                             the suite digest must cover only the suite's own bytes",
                            path_to_slash_string(&relative_entry)
                        ),
                    )
                    .into(),
                ));
            } else if fs.is_dir(&entry) {
                pending.push(relative_entry);
            } else if fs.is_file(&entry) {
                files.push(relative_entry);
            }
        }
    }
    Ok(files)
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A directory-authored suite has no single byte stream the way a manifest file does,
/// so its identity is a canonical rendering instead: case ids in sorted order and,
/// within a case, every file in sorted relative-path order, each hashed as
/// `path\0len\0bytes`. The regime tag at the front means this can never produce the
/// same digest as `sha256(manifest bytes)` for the same logical content, so a suite
/// that moves from one layout to the other is honestly reported as drifted.
fn canonical_directory_hash(fs: &impl FileSystem, evals_dir: &Path, case_ids: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(DIRECTORY_HASH_REGIME_TAG.as_bytes());

    for case_id in case_ids {
        let case_dir = evals_dir.join(case_id);
        let mut relative_paths: Vec<String> = collect_case_files(fs, &case_dir)?
            .iter()
            .map(|path| path_to_slash_string(path))
            .collect();
        relative_paths.sort();

        for relative in relative_paths {
            let bytes = fs.read_bytes(&case_dir.join(&relative))?;
            let entry_path = format!("{case_id}/{relative}");
            hasher.update(entry_path.as_bytes());
            hasher.update(b"\0");
            hasher.update(bytes.len().to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
        }
    }

    Ok(format!("sha256:{}", super::hex_encode(hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::MemFS;

    fn skill_md(fs: &MemFS, skill_dir: &str, name: &str) {
        fs.insert(
            Path::new(skill_dir).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A demo skill.\n---\n\n# Body\n"),
        );
    }

    #[test]
    fn a_suite_compiled_from_case_directories_carries_the_prompt_file_as_its_prompt() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.\n");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "The thing is done."}"#,
        );

        let compiled = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        assert_eq!(compiled.suite.evals.len(), 1);
        assert_eq!(compiled.suite.evals[0].id.as_str(), "one");
        assert_eq!(compiled.suite.evals[0].prompt.as_str(), "Do the thing.");
        assert_eq!(compiled.suite.evals[0].expected_output.as_str(), "The thing is done.");
        assert!(matches!(compiled.source, EvalSource::CaseDirectories { .. }));
    }

    #[test]
    fn a_case_directory_missing_prompt_md_is_refused_by_name() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "The thing is done."}"#,
        );

        let error = resolve_eval_suite(&fs, Path::new("/skill")).unwrap_err();

        assert!(error.to_string().contains("evals/one"));
        assert!(error.to_string().contains("prompt.md"));
    }

    #[test]
    fn a_case_json_that_redeclares_the_directory_derived_id_is_refused() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"id": "not-one", "expected_output": "done"}"#,
        );

        let error = resolve_eval_suite(&fs, Path::new("/skill")).unwrap_err();

        assert!(error.to_string().contains("'id'"));
    }

    #[test]
    fn a_grader_file_stem_becomes_its_default_name() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "done"}"#,
        );
        fs.insert(
            Path::new("/skill/evals/one/graders/mentions-total.json"),
            r#"{"type": "contains", "text": "total"}"#,
        );

        let compiled = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        let grader = &compiled.suite.evals[0].graders[0];
        assert_eq!(grader.name.as_ref().map(|n| n.as_str()), Some("mentions-total"));
    }

    #[test]
    fn mixing_a_manifest_with_case_directories_in_one_suite_is_refused() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(
            Path::new("/skill/evals/evals.json"),
            r#"{"skill_name": "demo-skill", "evals": [{"id": "manifest-case", "prompt": "p", "expected_output": "e"}]}"#,
        );
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "done"}"#,
        );

        let error = resolve_eval_suite(&fs, Path::new("/skill")).unwrap_err();

        assert!(error.to_string().contains("evals.json"));
        assert!(error.to_string().contains("one"));
    }

    #[test]
    fn a_directory_that_holds_only_unrelated_files_is_not_mistaken_for_a_case() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(
            Path::new("/skill/evals/evals.json"),
            r#"{"skill_name": "demo-skill", "evals": [{"id": "manifest-case", "prompt": "p", "expected_output": "e"}]}"#,
        );
        fs.insert(Path::new("/skill/evals/files/sales.csv"), "a,b\n1,2\n");

        let compiled = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        assert!(matches!(compiled.source, EvalSource::Manifest { .. }));
        assert_eq!(compiled.suite.evals[0].id.as_str(), "manifest-case");
    }

    #[test]
    fn the_manifest_hash_stays_the_raw_file_bytes_digest() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        let manifest =
            r#"{"skill_name": "demo-skill", "evals": [{"id": "one", "prompt": "p", "expected_output": "e"}]}"#;
        fs.insert(Path::new("/skill/evals/evals.json"), manifest);

        let compiled = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(manifest.as_bytes());
        let expected = format!("sha256:{}", super::super::hex_encode(hasher.finalize()));
        assert_eq!(compiled.hash, expected);
    }

    #[test]
    fn the_same_logical_case_hashes_differently_under_each_regime() {
        let manifest_fs = MemFS::new();
        skill_md(&manifest_fs, "/skill", "demo-skill");
        manifest_fs.insert(
            Path::new("/skill/evals/evals.json"),
            r#"{"skill_name": "demo-skill", "evals": [{"id": "one", "prompt": "Do the thing.", "expected_output": "The thing is done."}]}"#,
        );

        let directory_fs = MemFS::new();
        skill_md(&directory_fs, "/skill", "demo-skill");
        directory_fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        directory_fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "The thing is done."}"#,
        );

        let manifest_compiled = resolve_eval_suite(&manifest_fs, Path::new("/skill")).unwrap();
        let directory_compiled = resolve_eval_suite(&directory_fs, Path::new("/skill")).unwrap();

        assert_ne!(manifest_compiled.hash, directory_compiled.hash);
    }

    #[test]
    fn the_directory_hash_is_stable_across_repeated_resolutions() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "done"}"#,
        );

        let first = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();
        let second = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        assert_eq!(first.hash, second.hash);
    }

    #[test]
    fn the_directory_hash_changes_when_a_grader_file_changes() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");
        fs.insert(Path::new("/skill/evals/one/prompt.md"), "Do the thing.");
        fs.insert(
            Path::new("/skill/evals/one/case.json"),
            r#"{"expected_output": "done"}"#,
        );
        fs.insert(
            Path::new("/skill/evals/one/graders/mentions-total.json"),
            r#"{"type": "contains", "text": "total"}"#,
        );
        let before = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        fs.insert(
            Path::new("/skill/evals/one/graders/mentions-total.json"),
            r#"{"type": "contains", "text": "grand total"}"#,
        );
        let after = resolve_eval_suite(&fs, Path::new("/skill")).unwrap();

        assert_ne!(before.hash, after.hash);
    }

    #[test]
    fn a_skill_with_neither_a_manifest_nor_case_directories_fails_the_same_way_it_always_has() {
        let fs = MemFS::new();
        skill_md(&fs, "/skill", "demo-skill");

        let error = resolve_eval_suite(&fs, Path::new("/skill")).unwrap_err();

        assert!(matches!(error, EvalError::Io(_)));
    }

    // MemFS has no symlinks (see `FileSystem::is_symlink` there), so a symlink can only
    // be expressed against the real filesystem. Without the `is_symlink` check this
    // does not error: `fixtures/loop` points back at `fixtures`, and the walk chases it
    // until the OS's own symlink-resolution limit kicks in, at which point `is_dir` and
    // `is_file` (which swallow their errors) both read as false and the entry is
    // silently dropped, so `resolve_eval_suite` returns `Ok` with an incomplete digest
    // instead of rejecting the suite.
    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_a_case_directory_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let skill_dir = temp.path().join("skill");
        std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: A demo skill.\n---\n\n# Body\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("evals/one/prompt.md"), "Do the thing.\n").unwrap();
        std::fs::write(
            skill_dir.join("evals/one/case.json"),
            r#"{"expected_output": "The thing is done."}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(
            skill_dir.join("evals/one/fixtures"),
            skill_dir.join("evals/one/fixtures/loop"),
        )
        .unwrap();

        let error = resolve_eval_suite(&crate::fs::RealFS, &skill_dir).unwrap_err();

        match error {
            EvalError::Validation(errors) => {
                let message = errors.to_string();
                assert!(message.contains("symlink"), "{message}");
                assert!(message.contains("fixtures/loop"), "{message}");
            }
            other => panic!("expected a validation error naming the symlink, got {other:?}"),
        }
    }
}
