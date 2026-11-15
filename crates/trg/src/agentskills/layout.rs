//! Docs-compatible mirror layout for skill eval artifact bundles.
//!
//! Canonical run outputs stay under `runs/run-###/workspace`. Each report also
//! emits an `iteration-<N>/eval-<slug>/<scenario>/` tree that matches the
//! agentskills.io docs layout. Scenario directories are symlinks pointing at
//! the canonical workspace paths so outputs are not duplicated. When the
//! symlink call fails (e.g. read-only filesystem), a thin `.workspace-ref`
//! JSON file records the relative path to the canonical workspace instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::evals::{EvalCase, EvalError, EvalSuite, Result};
use super::report::{ReportBundle, RunRecord, ScenarioKind};

/// Maps docs-layout eval slugs to canonical run directories under the report bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasIndex {
    #[serde(flatten)]
    pub evals: HashMap<String, EvalAliasEntry>,
}

/// Per-eval-case mapping from scenario (and optional attempt) to a `runs/run-NNN` path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalAliasEntry {
    #[serde(flatten)]
    pub scenarios: HashMap<String, ScenarioAlias>,
}

/// Single run path, or per-attempt paths when `--attempts` > 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScenarioAlias {
    Single(String),
    Multi(HashMap<String, String>),
}

pub fn iteration_dir_name(iteration: u32) -> String {
    format!("iteration-{iteration}")
}

pub fn eval_dir_name(slug: &str) -> String {
    format!("eval-{slug}")
}

/// Docs mirror path relative to the report directory (trailing slash).
///
/// When `attempts` is 1, layout matches agentskills.io: `iteration-N/eval-X/scenario/`.
/// With multiple attempts, each attempt is nested under `attempt-K/`.
pub fn scenario_mirror_path(iteration: u32, eval_slug: &str, scenario: &str, attempt: u32, attempts: u32) -> String {
    let base = format!(
        "{}/{}/{}/",
        iteration_dir_name(iteration),
        eval_dir_name(eval_slug),
        scenario
    );
    if attempts <= 1 {
        base
    } else {
        format!("{base}attempt-{attempt}/")
    }
}

/// Derive a stable, filesystem-safe slug from an eval case ID.
///
/// Rules: lowercase; path separators, whitespace, and other non-alphanumeric
/// characters become a single `-`; repeated separators collapse; leading and
/// trailing `-` are trimmed. Empty results fall back to `"eval"`.
pub fn eval_slug(eval_id: &str) -> String {
    let mut slug = String::new();
    let mut prev_hyphen = false;

    for ch in eval_id.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
            prev_hyphen = false;
        } else if !prev_hyphen && !slug.is_empty() {
            slug.push('-');
            prev_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "eval".to_string()
    } else {
        slug
    }
}

/// Assign unique slugs for every eval case, disambiguating collisions with `-2`, `-3`, …
pub fn assign_eval_slugs(evals: &[EvalCase]) -> HashMap<String, String> {
    let mut slug_usage: HashMap<String, usize> = HashMap::new();
    let mut slugs = HashMap::new();

    for eval_case in evals {
        let base = eval_slug(eval_case.id.as_str());
        let usage = slug_usage.entry(base.clone()).or_insert(0);
        *usage += 1;
        let slug = if *usage == 1 { base } else { format!("{base}-{usage}") };
        slugs.insert(eval_case.id.to_string(), slug);
    }

    slugs
}

/// Scan existing report bundles under `out_root/skill_name` and return the next iteration.
pub fn detect_next_iteration(out_root: &Path, skill_name: &str) -> u32 {
    let skill_root = out_root.join(skill_name);
    let mut max_iteration = 0u32;

    let entries = match std::fs::read_dir(&skill_root) {
        Ok(entries) => entries,
        Err(_) => return 1,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if let Ok(iteration) = iteration_from_dir_name(path.file_name().and_then(|name| name.to_str())) {
            max_iteration = max_iteration.max(iteration);
            continue;
        }

        if let Ok(report_entries) = std::fs::read_dir(&path) {
            for report_entry in report_entries.flatten() {
                if let Some(name) = report_entry.file_name().to_str() {
                    if let Ok(iteration) = iteration_from_dir_name(Some(name)) {
                        max_iteration = max_iteration.max(iteration);
                    }
                }
            }
        }
    }

    max_iteration.saturating_add(1).max(1)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This directory-name probe classifies non-iteration entries as a unit result; numeric parse diagnostics are not part of that classification"
    )
)]
fn iteration_from_dir_name(name: Option<&str>) -> std::result::Result<u32, ()> {
    let name = name.ok_or(())?;
    let number = name.strip_prefix("iteration-").ok_or(())?;
    number.parse().map_err(|_| ())
}

/// Returns an error when iteration `N` already exists under `out_root/skill_name`.
pub fn ensure_iteration_available(
    out_root: &Path,
    skill_name: &str,
    iteration: u32,
    force: bool,
    exclude_report_dir: Option<&Path>,
) -> Result<()> {
    if force {
        return Ok(());
    }

    let iteration_name = iteration_dir_name(iteration);
    let skill_root = out_root.join(skill_name);

    if iteration_exists(&skill_root.join(&iteration_name), exclude_report_dir) {
        return iteration_exists_error(&skill_root.join(&iteration_name), iteration);
    }

    let entries = match std::fs::read_dir(&skill_root) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let report_dir = entry.path();
        if !report_dir.is_dir() {
            continue;
        }

        if Some(report_dir.as_path()) == exclude_report_dir {
            continue;
        }

        let iteration_dir = report_dir.join(&iteration_name);
        if iteration_exists(&iteration_dir, exclude_report_dir) {
            return iteration_exists_error(&iteration_dir, iteration);
        }
    }

    Ok(())
}

fn iteration_exists(iteration_dir: &Path, exclude_report_dir: Option<&Path>) -> bool {
    if !iteration_dir.is_dir() {
        return false;
    }

    if let Some(exclude) = exclude_report_dir {
        if iteration_dir.starts_with(exclude) {
            return false;
        }
    }

    true
}

fn iteration_exists_error(iteration_dir: &Path, iteration: u32) -> Result<()> {
    Err(EvalError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "iteration {iteration} already exists at {} (pass --force to overwrite)",
            iteration_dir.display()
        ),
    )))
}

/// How many draws each (case, scenario) cell of a report actually took.
///
/// Read back from the runs rather than taken from the operator's `--attempts`, because a
/// case can pin its own count. Keyed on the cell and not on the pass, so a case drawn once
/// still lands in an unsuffixed scenario directory even when another case in the same pass
/// was drawn five times.
pub struct DrawsPerCell(HashMap<(String, ScenarioKind), u32>);

impl DrawsPerCell {
    pub fn of(runs: &[RunRecord]) -> Self {
        let mut draws: HashMap<(String, ScenarioKind), u32> = HashMap::new();
        for run in runs {
            let cell = (run.eval_case_id.clone(), run.scenario_id);
            let highest = draws.entry(cell).or_insert(run.attempt);
            *highest = (*highest).max(run.attempt);
        }
        Self(draws)
    }

    pub fn of_run(&self, run: &RunRecord) -> u32 {
        self.0
            .get(&(run.eval_case_id.clone(), run.scenario_id))
            .copied()
            .unwrap_or(1)
    }
}

pub fn build_alias_index(bundle: &ReportBundle, slugs: &HashMap<String, String>, draws: &DrawsPerCell) -> AliasIndex {
    let mut evals: HashMap<String, EvalAliasEntry> = HashMap::new();

    for run in &bundle.document.runs {
        let slug = slugs
            .get(&run.eval_case_id)
            .cloned()
            .unwrap_or_else(|| eval_slug(&run.eval_case_id));
        let eval_key = eval_dir_name(&slug);
        let scenario_key = run.scenario_id.as_str().to_string();
        let run_dir = format!("runs/{}", run.id);

        let entry = evals.entry(eval_key).or_insert_with(|| EvalAliasEntry {
            scenarios: HashMap::new(),
        });

        if draws.of_run(run) <= 1 {
            entry.scenarios.insert(scenario_key, ScenarioAlias::Single(run_dir));
        } else {
            let attempt_key = format!("attempt-{}", run.attempt);
            match entry.scenarios.get_mut(&scenario_key) {
                Some(ScenarioAlias::Multi(map)) => {
                    map.insert(attempt_key, run_dir);
                }
                Some(ScenarioAlias::Single(_)) => {
                    let mut map = HashMap::new();
                    map.insert(attempt_key, run_dir);
                    entry.scenarios.insert(scenario_key, ScenarioAlias::Multi(map));
                }
                None => {
                    let mut map = HashMap::new();
                    map.insert(attempt_key, run_dir);
                    entry.scenarios.insert(scenario_key, ScenarioAlias::Multi(map));
                }
            }
        }
    }

    AliasIndex { evals }
}

pub fn write_alias_index(report_dir: &Path, iteration: u32, index: &AliasIndex) -> Result<()> {
    let iteration_dir = report_dir.join(iteration_dir_name(iteration));
    std::fs::create_dir_all(&iteration_dir)?;
    let payload = serde_json::to_string_pretty(index)?;
    std::fs::write(iteration_dir.join("alias-index.json"), payload)?;
    Ok(())
}

pub fn write_docs_mirror_layout(
    report_dir: &Path,
    bundle: &ReportBundle,
    iteration: u32,
    slugs: &HashMap<String, String>,
) -> Result<()> {
    let iteration_dir = report_dir.join(iteration_dir_name(iteration));
    std::fs::create_dir_all(&iteration_dir)?;

    let benchmark_path = iteration_dir.join("benchmark.json");
    if !benchmark_path.exists() {
        std::fs::write(&benchmark_path, "{}\n")?;
    }

    let draws = DrawsPerCell::of(&bundle.document.runs);

    let alias_index = build_alias_index(bundle, slugs, &draws);
    write_alias_index(report_dir, iteration, &alias_index)?;

    for run in &bundle.document.runs {
        let slug = slugs
            .get(&run.eval_case_id)
            .cloned()
            .unwrap_or_else(|| eval_slug(&run.eval_case_id));
        write_scenario_mirror(report_dir, &iteration_dir, run, &slug, draws.of_run(run))?;
    }

    Ok(())
}

fn write_scenario_mirror(
    report_dir: &Path,
    iteration_dir: &Path,
    run: &RunRecord,
    slug: &str,
    draws: u32,
) -> Result<()> {
    let mirror_rel = run.mirror_path.trim_end_matches('/');
    let scenario_dir = if mirror_rel.starts_with("iteration-") {
        report_dir.join(mirror_rel)
    } else {
        let mut path = iteration_dir.join(eval_dir_name(slug)).join(run.scenario_id.as_str());
        if draws > 1 {
            path = path.join(format!("attempt-{}", run.attempt));
        }
        path
    };

    if scenario_dir.exists() {
        remove_path(&scenario_dir)?;
    }
    if let Some(parent) = scenario_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let canonical_workspace = report_dir.join(&run.paths.workspace);
    link_to_workspace(&scenario_dir, report_dir, &canonical_workspace);

    Ok(())
}

/// Symlink the scenario leaf to the canonical workspace.
///
/// Failures are logged to stderr and a `.workspace-ref` fallback is written so exotic
/// filesystems (e.g. read-only or non-symlink-capable) still get `alias-index.json`.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn link_to_workspace(link_path: &Path, report_dir: &Path, workspace: &Path) {
    let relative = match workspace.strip_prefix(report_dir) {
        Ok(path) => path.to_path_buf(),
        Err(_) => {
            eprintln!(
                "docs mirror: workspace path {} is not under report directory {}; skipping symlink",
                workspace.display(),
                report_dir.display()
            );
            return;
        }
    };

    let Some(link_dir) = link_path.parent() else {
        eprintln!(
            "docs mirror: link path {} has no parent directory; skipping symlink",
            link_path.display()
        );
        return;
    };
    let relative_link = relative_path_from(link_dir, report_dir, &relative);
    if let Err(error) = std::os::unix::fs::symlink(&relative_link, link_path) {
        eprintln!(
            "docs mirror: symlink {} -> {} failed ({error}); writing .workspace-ref fallback",
            link_path.display(),
            relative_link.display(),
        );
        write_workspace_ref(link_path, &relative);
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn write_workspace_ref(link_path: &Path, workspace_relative: &Path) {
    if let Err(error) = std::fs::create_dir_all(link_path) {
        eprintln!(
            "docs mirror: failed to create scenario directory {} ({error})",
            link_path.display()
        );
        return;
    }
    let ref_path = link_path.join(".workspace-ref");
    #[derive(serde::Serialize)]
    struct WorkspaceReference<'a> {
        workspace: std::borrow::Cow<'a, str>,
    }
    let payload = WorkspaceReference {
        workspace: workspace_relative.to_string_lossy(),
    };
    let serialized = match serde_json::to_string_pretty(&payload) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("docs mirror: failed to serialize {} ({error})", ref_path.display());
            return;
        }
    };
    if let Err(error) = std::fs::write(&ref_path, serialized) {
        eprintln!("docs mirror: failed to write {} ({error})", ref_path.display());
    }
}

fn relative_path_from(from_dir: &Path, report_dir: &Path, to_relative: &Path) -> PathBuf {
    let depth = from_dir
        .strip_prefix(report_dir)
        .map(|relative| relative.components().count())
        .unwrap_or(0);

    let mut result = PathBuf::new();
    for _ in 0..depth {
        result.push("..");
    }
    result.push(to_relative);
    result
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = path.symlink_metadata().or_else(|_| std::fs::metadata(path))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn slugs_for_suite(suite: &EvalSuite) -> HashMap<String, String> {
    assign_eval_slugs(&suite.evals)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
