//! Resolving an eval suite from either its flat manifest or a directory-per-case
//! layout, so every caller runs and verifies the same definition.

mod constants;
pub use constants::EVAL_CASE_PROMPT_FILE_NAME;
use constants::{CASE_JSON_NAME, DIRECTORY_HASH_REGIME_TAG, GRADERS_DIR_NAME};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::evals::{parse_eval_suite, EvalCase, EvalDirName, EvalError, EvalSuite, Result, EVAL_SUITE_MANIFEST_NAME};
use super::validation::ValidationError;
use crate::fs::FileSystem;

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
///
/// `eval_dir` is the caller's own answer to where the suite lives (a `--eval-dir` flag,
/// or the default), and it is what locates the suite: a manifest's own `eval_dir` key,
/// once found, is checked against it rather than used to redirect the search, because a
/// key that could redirect discovery would make the directory the manifest is found in
/// depend on bytes read from inside that same directory.
pub fn resolve_eval_suite(fs: &impl FileSystem, skill_path: &Path, eval_dir: &EvalDirName) -> Result<CompiledSuite> {
    let evals_dir = skill_path.join(eval_dir.as_str());
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
    if let Some(declared) = &suite.eval_dir {
        if declared != eval_dir {
            return Err(mismatched_eval_dir_error(&manifest_path, declared, eval_dir));
        }
    }
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

fn mismatched_eval_dir_error(manifest_path: &Path, declared: &EvalDirName, resolved: &EvalDirName) -> EvalError {
    EvalError::Validation(
        ValidationError::for_field(
            "eval_dir",
            format!(
                "manifest at '{}' declares eval_dir '{}', but it was found by looking in '{}'; \
                 pass --eval-dir {} or correct the manifest so the two agree",
                manifest_path.display(),
                declared,
                resolved,
                declared
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
        let looks_like_a_case = fs.is_file(&entry.join(EVAL_CASE_PROMPT_FILE_NAME))
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

    #[derive(serde::Serialize)]
    struct SuiteDocument {
        skill_name: String,
        evals: Vec<serde_json::Value>,
    }
    let suite_value = serde_json::to_value(SuiteDocument { skill_name, evals })?;
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
    let prompt_path = case_dir.join(EVAL_CASE_PROMPT_FILE_NAME);
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
#[path = "case_directories_tests.rs"]
mod tests;
