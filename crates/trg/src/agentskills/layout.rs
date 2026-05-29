//! Docs-compatible mirror layout for skill eval artifact bundles.
//!
//! Canonical run outputs stay under `runs/run-###/workspace`. Each report also
//! emits an `iteration-<N>/eval-<slug>/<scenario>/` tree that matches the
//! agentskills.io docs layout. Scenario directories are **symlinks** on Unix
//! pointing at the canonical workspace paths so outputs are not duplicated.
//! When symlinks are unavailable, a thin `.workspace-ref` JSON file records
//! the relative path to the canonical workspace instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::evals::{EvalCase, EvalError, EvalSuite, Result};
use super::report::{ReportBundle, RunRecord};

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

pub fn build_alias_index(bundle: &ReportBundle, slugs: &HashMap<String, String>, attempts: u32) -> AliasIndex {
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

        if attempts <= 1 {
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

    let attempts = bundle.document.runs.iter().map(|run| run.attempt).max().unwrap_or(1);

    let alias_index = build_alias_index(bundle, slugs, attempts);
    write_alias_index(report_dir, iteration, &alias_index)?;

    for run in &bundle.document.runs {
        let slug = slugs
            .get(&run.eval_case_id)
            .cloned()
            .unwrap_or_else(|| eval_slug(&run.eval_case_id));
        write_scenario_mirror(report_dir, &iteration_dir, run, &slug, attempts)?;
    }

    Ok(())
}

fn write_scenario_mirror(
    report_dir: &Path,
    iteration_dir: &Path,
    run: &RunRecord,
    slug: &str,
    attempts: u32,
) -> Result<()> {
    let mirror_rel = run.mirror_path.trim_end_matches('/');
    let scenario_dir = if mirror_rel.starts_with("iteration-") {
        report_dir.join(mirror_rel)
    } else {
        let mut path = iteration_dir.join(eval_dir_name(slug)).join(run.scenario_id.as_str());
        if attempts > 1 {
            path = path.join(format!("attempt-{}", run.attempt));
        }
        path
    };

    if scenario_dir.exists() {
        remove_path(&scenario_dir)?;
    }
    std::fs::create_dir_all(scenario_dir.parent().unwrap())?;

    let canonical_workspace = report_dir.join(&run.paths.workspace);
    link_to_workspace(&scenario_dir, report_dir, &canonical_workspace);

    Ok(())
}

/// Symlink the scenario leaf to the canonical workspace when supported; otherwise no-op.
///
/// Failures are logged to stderr and ignored so exotic filesystems still get `alias-index.json`.
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

    #[cfg(unix)]
    {
        let relative_link = relative_path_from(link_path.parent().unwrap(), report_dir, &relative);
        if let Err(error) = std::os::unix::fs::symlink(&relative_link, link_path) {
            eprintln!(
                "docs mirror: symlink {} -> {} failed ({error}); writing .workspace-ref fallback",
                link_path.display(),
                relative_link.display(),
            );
            write_workspace_ref(link_path, &relative);
        }
    }

    #[cfg(not(unix))]
    {
        write_workspace_ref(link_path, &relative);
    }
}

fn write_workspace_ref(link_path: &Path, workspace_relative: &Path) {
    if let Err(error) = std::fs::create_dir_all(link_path) {
        eprintln!(
            "docs mirror: failed to create scenario directory {} ({error})",
            link_path.display()
        );
        return;
    }
    let ref_path = link_path.join(".workspace-ref");
    let payload = serde_json::json!({
        "workspace": workspace_relative.to_string_lossy(),
    });
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
mod tests {
    use super::*;
    use crate::agentskills::evals::{EvalCase, EvalSuite};
    use crate::fs::testutil::MemFS;
    use crate::fs::FileSystem;
    use std::path::Path;

    fn sample_evals(ids: &[&str]) -> Vec<EvalCase> {
        ids.iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id,
                    "prompt": "prompt",
                    "expected_output": "output",
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn eval_slug_normalizes_path_separators_and_whitespace() {
        assert_eq!(eval_slug("foo/bar"), "foo-bar");
        assert_eq!(eval_slug("foo//bar"), "foo-bar");
        assert_eq!(eval_slug("My Eval Case"), "my-eval-case");
        assert_eq!(eval_slug("  spaced  "), "spaced");
        assert_eq!(eval_slug("@#$start"), "start");
    }

    #[test]
    fn eval_slug_preserves_unicode_letters() {
        assert_eq!(eval_slug("Café Résumé"), "café-résumé");
        assert_eq!(eval_slug("日本語"), "日本語");
    }

    #[test]
    fn eval_slug_disambiguates_collisions() {
        let evals = sample_evals(&["a/b", "a b", "a-b"]);
        let slugs = assign_eval_slugs(&evals);
        assert_eq!(slugs.get("a/b").map(String::as_str), Some("a-b"));
        assert_eq!(slugs.get("a b").map(String::as_str), Some("a-b-2"));
        assert_eq!(slugs.get("a-b").map(String::as_str), Some("a-b-3"));
    }

    #[test]
    fn detect_next_iteration_scans_report_bundles() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("demo-skill");
        std::fs::create_dir_all(skill_root.join("report-a").join("iteration-1")).unwrap();
        std::fs::create_dir_all(skill_root.join("report-b").join("iteration-3")).unwrap();

        assert_eq!(detect_next_iteration(temp.path(), "demo-skill"), 4);
    }

    #[test]
    fn detect_next_iteration_defaults_to_one() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(detect_next_iteration(temp.path(), "missing-skill"), 1);
    }

    #[test]
    fn ensure_iteration_available_rejects_existing_iteration() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("demo-skill");
        let report_dir = skill_root.join("report-a");
        std::fs::create_dir_all(report_dir.join("iteration-2")).unwrap();

        let err = ensure_iteration_available(temp.path(), "demo-skill", 2, false, None).unwrap_err();
        assert!(err.to_string().contains("iteration 2 already exists"));
    }

    #[test]
    fn ensure_iteration_available_allows_force() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("demo-skill");
        std::fs::create_dir_all(skill_root.join("report-a").join("iteration-2")).unwrap();

        ensure_iteration_available(temp.path(), "demo-skill", 2, true, None).unwrap();
    }

    #[test]
    fn alias_index_round_trips_and_references_existing_runs() {
        use crate::agentskills::report::{BuildReportOptions, ScenarioKind};

        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = Path::new("demo-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case/a", "prompt": "p", "expected_output": "o", "assertions": ["a"] }
                ]
            }"#,
        );

        let bundle = crate::agentskills::report::build_report_bundle(
            &fs,
            skill_path,
            skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions {
                report_id: Some("alias-report".to_string()),
                iteration: Some(1),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let slugs = slugs_for_suite(
            &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap())
                .unwrap(),
        );

        let report_dir = temp.path().join("demo-skill/alias-report");
        std::fs::create_dir_all(&report_dir).unwrap();
        for workspace in &bundle.workspace_dirs {
            std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
        }

        write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

        let index_path = report_dir.join("iteration-1/alias-index.json");
        assert!(index_path.is_file());
        let raw = std::fs::read_to_string(&index_path).unwrap();
        let index: AliasIndex = serde_json::from_str(&raw).unwrap();
        let round_trip: AliasIndex = serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
        assert_eq!(index, round_trip);

        let entry = index.evals.get("eval-case-a").expect("eval-case-a entry");
        match entry.scenarios.get("with_skill").unwrap() {
            ScenarioAlias::Single(path) => {
                assert_eq!(path, "runs/run-001");
                assert!(report_dir.join(path).join("workspace").is_dir());
            }
            ScenarioAlias::Multi(_) => panic!("expected single attempt mapping"),
        }
        match entry.scenarios.get("without_skill").unwrap() {
            ScenarioAlias::Single(path) => {
                assert_eq!(path, "runs/run-002");
                assert!(report_dir.join(path).join("workspace").is_dir());
            }
            ScenarioAlias::Multi(_) => panic!("expected single attempt mapping"),
        }
    }

    #[test]
    fn mirror_layout_disambiguates_multiple_attempts() {
        use crate::agentskills::report::{BuildReportOptions, ScenarioKind};

        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = Path::new("demo-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "one", "prompt": "p", "expected_output": "o", "assertions": ["a"] }
                ]
            }"#,
        );

        let bundle = crate::agentskills::report::build_report_bundle(
            &fs,
            skill_path,
            skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("attempts-report".to_string()),
                iteration: Some(1),
                attempts: 2,
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let slugs = slugs_for_suite(
            &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap())
                .unwrap(),
        );

        let report_dir = temp.path().join("demo-skill/attempts-report");
        std::fs::create_dir_all(&report_dir).unwrap();
        for workspace in &bundle.workspace_dirs {
            std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
        }

        write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

        let attempt_one = report_dir.join("iteration-1/eval-one/with_skill/attempt-1");
        let attempt_two = report_dir.join("iteration-1/eval-one/with_skill/attempt-2");
        assert!(attempt_one.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(attempt_two.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&attempt_one).unwrap(),
            PathBuf::from("../../../runs/run-001/workspace")
        );
        assert_eq!(
            std::fs::read_link(&attempt_two).unwrap(),
            PathBuf::from("../../../runs/run-002/workspace")
        );

        let index: AliasIndex =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("iteration-1/alias-index.json")).unwrap())
                .unwrap();
        match index
            .evals
            .get("eval-one")
            .unwrap()
            .scenarios
            .get("with_skill")
            .unwrap()
        {
            ScenarioAlias::Multi(map) => {
                assert_eq!(map.get("attempt-1").map(String::as_str), Some("runs/run-001"));
                assert_eq!(map.get("attempt-2").map(String::as_str), Some("runs/run-002"));
            }
            ScenarioAlias::Single(_) => panic!("expected multi attempt mapping"),
        }
    }

    #[test]
    fn mirror_layout_snapshot() {
        use crate::agentskills::report::{BuildReportOptions, ScenarioKind};
        use crate::fs::testutil::MemFS;

        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = Path::new("demo-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: d\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case/a", "prompt": "p", "expected_output": "o", "assertions": ["a"] },
                    { "id": "2", "prompt": "p2", "expected_output": "o2", "assertions": ["b"] }
                ]
            }"#,
        );

        let bundle = crate::agentskills::report::build_report_bundle(
            &fs,
            skill_path,
            skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions {
                report_id: Some("snapshot-report".to_string()),
                generated_at: Some("2026-05-26T00:00:00Z".to_string()),
                iteration: Some(1),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let slugs = slugs_for_suite(
            &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap())
                .unwrap(),
        );

        let report_dir = temp.path().join("demo-skill/snapshot-report");
        std::fs::create_dir_all(&report_dir).unwrap();
        for workspace in &bundle.workspace_dirs {
            std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
        }

        write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

        assert!(report_dir.join("iteration-1/benchmark.json").is_file());

        let with_skill = report_dir.join("iteration-1/eval-case-a/with_skill");
        let without_skill = report_dir.join("iteration-1/eval-case-a/without_skill");
        let eval_two_with = report_dir.join("iteration-1/eval-2/with_skill");
        let eval_two_without = report_dir.join("iteration-1/eval-2/without_skill");

        for link in [with_skill, without_skill, eval_two_with, eval_two_without] {
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "expected symlink at {}",
                link.display()
            );
        }

        assert_eq!(
            std::fs::read_link(report_dir.join("iteration-1/eval-case-a/with_skill")).unwrap(),
            PathBuf::from("../../runs/run-001/workspace")
        );
        assert_eq!(
            std::fs::read_link(report_dir.join("iteration-1/eval-case-a/without_skill")).unwrap(),
            PathBuf::from("../../runs/run-002/workspace")
        );
        assert_eq!(
            std::fs::read_link(report_dir.join("iteration-1/eval-2/with_skill")).unwrap(),
            PathBuf::from("../../runs/run-003/workspace")
        );
        assert_eq!(
            std::fs::read_link(report_dir.join("iteration-1/eval-2/without_skill")).unwrap(),
            PathBuf::from("../../runs/run-004/workspace")
        );
    }
}
