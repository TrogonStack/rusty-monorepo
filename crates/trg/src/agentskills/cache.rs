use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::report::{EnvironmentPolicy, RunRecord, ScenarioKind, SkillStaging};
use super::runner::Runner;

pub use super::prompt::PROMPT_CONTRACT_VERSION;

const POINTER_FILE: &str = "pointer.json";
const REUSE_DIR: &str = "reuse";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_input(input: &CacheKeyInput) -> Self {
        let json = serde_json::to_string(input).expect("CacheKeyInput serializes");
        Self(sha256_hex(&json))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureHash(String);

impl FixtureHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn empty() -> Self {
        Self(sha256_digest(""))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerVersion(String);

impl RunnerVersion {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKeyInput {
    pub eval_case_id: String,
    pub skill_hash: String,
    pub evals_hash: String,
    pub fixture_hash: String,
    pub model_config: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_model: Option<String>,
    pub runner_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
    pub scenario: ScenarioKind,
    /// Which draw of this cell the run is.
    ///
    /// `--attempts` asks for the same cell more than once precisely because one run of a
    /// non-deterministic agent says little about it. Without this the second draw is
    /// served the first one's artifacts, and the spread the benchmark reports across
    /// attempts is zero by construction rather than by measurement.
    pub attempt: u32,
    pub prompt_contract_version: String,
    /// What the run was allowed to see of this machine and of the live skill tree.
    ///
    /// Both decide what the harness could read while it worked, so a completed run
    /// answers only for the pair it ran under. Serving it to another pair would
    /// report a policy the run never ran with.
    #[serde(default)]
    pub environment: EnvironmentPolicy,
    #[serde(default)]
    pub skill_staging: SkillStaging,
    /// What the case's workspace scaffold said, for a case that declares one.
    ///
    /// The script decides the directory the run worked in, and `skill_hash` is the digest of
    /// `SKILL.md` alone, so nothing else here would notice the script being edited. A case
    /// that declares no scaffold serializes as it always did, which leaves entries written
    /// before this field readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaffold_hash: Option<String>,
}

/// What a completed run has to match to be served to a run that asked for any completed
/// one.
///
/// `--reuse-completed` deliberately forgets the model config, the runner, and the version
/// of it a run was produced with, because an operator iterating on a skill wants each case
/// answered once. It cannot forget the scenario, and it cannot forget which draw of the
/// cell it is looking at. Two arms of the same case differ in nothing else, so serving one
/// arm's run to the other compares a run against itself and reports a delta of zero for a
/// skill that was never exercised; two attempts of one arm differ in nothing at all, and
/// the point of asking for a second one is to get a second sample rather than a copy.
///
/// It cannot forget the scaffold either, for the same reason it cannot forget the fixtures:
/// both decide what was in the directory the run worked in, and a run answers only for the
/// directory it was handed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReuseKeyInput {
    pub eval_case_id: String,
    pub skill_hash: String,
    pub evals_hash: String,
    pub fixture_hash: String,
    pub scenario: ScenarioKind,
    pub attempt: u32,
    #[serde(default)]
    pub scaffold_hash: Option<String>,
}

impl ReuseKeyInput {
    pub fn of(key_input: &CacheKeyInput) -> Self {
        Self {
            eval_case_id: key_input.eval_case_id.clone(),
            skill_hash: key_input.skill_hash.clone(),
            evals_hash: key_input.evals_hash.clone(),
            fixture_hash: key_input.fixture_hash.clone(),
            scenario: key_input.scenario,
            attempt: key_input.attempt,
            scaffold_hash: key_input.scaffold_hash.clone(),
        }
    }

    /// The directory this run's reuse pointer lives in.
    ///
    /// One slot per case, scenario and attempt. A single slot per case would let each arm
    /// evict the other's pointer, so a comparison would re-execute every run it was told
    /// to reuse.
    fn slot(&self, out_dir: &Path) -> PathBuf {
        cache_root(out_dir)
            .join(REUSE_DIR)
            .join(sanitize_dir_name(&self.eval_case_id))
            .join(self.scenario.as_str())
            .join(format!("attempt-{}", self.attempt))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunCacheInfo {
    pub hit: bool,
    pub source_run_id: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachePointer {
    pub report_dir: String,
    pub run_id: String,
    pub key_input: CacheKeyInput,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CacheOptions {
    pub enabled: bool,
    pub reuse_completed: bool,
}

pub fn compute_fixture_hash(skill_path: &Path, eval_id: &str) -> io::Result<FixtureHash> {
    let fixtures_dir = skill_path.join("evals").join(eval_id).join("fixtures");
    if !fixtures_dir.is_dir() {
        return Ok(FixtureHash::empty());
    }

    let mut entries = Vec::new();
    collect_fixture_entries(&fixtures_dir, &fixtures_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut payload = String::new();
    for (relative_path, content_hash) in entries {
        payload.push_str(&relative_path);
        payload.push('\0');
        payload.push_str(&content_hash);
        payload.push('\n');
    }

    Ok(FixtureHash(sha256_digest(&payload)))
}

fn collect_fixture_entries(root: &Path, dir: &Path, entries: &mut Vec<(String, String)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_fixture_entries(root, &path, entries)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path)?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            entries.push((relative, sha256_digest_bytes(&bytes)));
        }
    }
    Ok(())
}

pub fn detect_runner_version(runner: Runner) -> Option<RunnerVersion> {
    let program = runner_program(runner);
    let output = std::process::Command::new(program).arg("--version").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let version = text.lines().next()?.trim();
    if version.is_empty() {
        None
    } else {
        Some(RunnerVersion(version.to_string()))
    }
}

pub fn try_resolve_cache(
    out_dir: &Path,
    options: CacheOptions,
    key_input: &CacheKeyInput,
    reuse_input: &ReuseKeyInput,
) -> Option<CachePointer> {
    if !options.enabled {
        return None;
    }
    let key = CacheKey::from_input(key_input);
    if let Some(pointer) = lookup_exact(out_dir, &key, key_input) {
        return Some(pointer);
    }
    if options.reuse_completed {
        return lookup_reuse(out_dir, reuse_input);
    }
    None
}

pub fn runner_kind_label(runner: Runner) -> &'static str {
    runner_program(runner)
}

fn runner_program(runner: Runner) -> &'static str {
    match runner {
        Runner::Codex => "codex",
        Runner::ClaudeCode => "claude",
        Runner::CursorAgent => "cursor-agent",
    }
}

pub fn cache_root(out_dir: &Path) -> PathBuf {
    out_dir.join(".cache")
}

pub fn lookup_exact(out_dir: &Path, key: &CacheKey, current: &CacheKeyInput) -> Option<CachePointer> {
    read_pointer_if_fresh(&cache_root(out_dir).join(key.as_str()), current)
}

pub fn lookup_reuse(out_dir: &Path, current: &ReuseKeyInput) -> Option<CachePointer> {
    read_reuse_pointer_if_fresh(&current.slot(out_dir), current)
}

fn read_pointer_if_fresh(dir: &Path, current: &CacheKeyInput) -> Option<CachePointer> {
    let pointer = read_pointer(dir)?;
    if pointer.key_input == *current {
        Some(pointer)
    } else {
        let _ = fs::remove_dir_all(dir);
        None
    }
}

fn read_reuse_pointer_if_fresh(dir: &Path, current: &ReuseKeyInput) -> Option<CachePointer> {
    let pointer = read_pointer(dir)?;
    if ReuseKeyInput::of(&pointer.key_input) == *current {
        Some(pointer)
    } else {
        let _ = fs::remove_dir_all(dir);
        None
    }
}

fn read_pointer(dir: &Path) -> Option<CachePointer> {
    let path = dir.join(POINTER_FILE);
    let content = fs::read_to_string(path).ok()?;
    let pointer: CachePointer = serde_json::from_str(&content).ok()?;
    let report_dir = PathBuf::from(&pointer.report_dir);
    if !report_dir.join("report.json").is_file() {
        let _ = fs::remove_dir_all(dir);
        return None;
    }
    Some(pointer)
}

pub fn record_completion(
    out_dir: &Path,
    key: &CacheKey,
    key_input: &CacheKeyInput,
    report_dir: &Path,
    run_id: &str,
) -> io::Result<()> {
    let pointer = CachePointer {
        report_dir: report_dir.to_string_lossy().into_owned(),
        run_id: run_id.to_string(),
        key_input: key_input.clone(),
    };
    write_pointer(&cache_root(out_dir).join(key.as_str()), &pointer)?;
    write_pointer(&ReuseKeyInput::of(key_input).slot(out_dir), &pointer)
}

fn write_pointer(dir: &Path, pointer: &CachePointer) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let json = serde_json::to_string_pretty(pointer).expect("CachePointer serializes");
    fs::write(dir.join(POINTER_FILE), json)
}

pub fn apply_cache_hit(
    run: &mut RunRecord,
    key: &CacheKey,
    pointer: &CachePointer,
    report_dir: &Path,
) -> io::Result<()> {
    let source_report = PathBuf::from(&pointer.report_dir);
    let source_run = load_source_run(&source_report, &pointer.run_id)?;
    copy_run_artifacts(&source_report, &pointer.run_id, report_dir, &run.id)?;

    run.status = source_run.status;
    run.metrics = source_run.metrics;
    run.artifacts = source_run
        .artifacts
        .into_iter()
        .map(|artifact| rewrite_artifact_run_id(artifact, &pointer.run_id, &run.id))
        .collect();
    run.skill_integrity = source_run.skill_integrity;
    run.cache = Some(RunCacheInfo {
        hit: true,
        source_run_id: pointer.run_id.clone(),
        key: key.as_str().to_string(),
    });
    Ok(())
}

fn load_source_run(source_report: &Path, run_id: &str) -> io::Result<RunRecord> {
    let content = fs::read_to_string(source_report.join("report.json"))?;
    let document: super::report::ReportDocument =
        serde_json::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    document
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("run {run_id} not found")))
}

fn rewrite_artifact_run_id(
    mut artifact: serde_json::Value,
    source_run_id: &str,
    dest_run_id: &str,
) -> serde_json::Value {
    let from = format!("runs/{source_run_id}/");
    let to = format!("runs/{dest_run_id}/");
    if let Some(object) = artifact.as_object_mut() {
        for (_, value) in object.iter_mut() {
            if let Some(text) = value.as_str() {
                if text.starts_with(&from) {
                    *value = serde_json::Value::String(text.replacen(&from, &to, 1));
                }
            }
        }
    }
    artifact
}

fn copy_run_artifacts(
    source_report: &Path,
    source_run_id: &str,
    dest_report: &Path,
    dest_run_id: &str,
) -> io::Result<()> {
    let source_run_dir = source_report.join("runs").join(source_run_id);
    let dest_run_dir = dest_report.join("runs").join(dest_run_id);
    fs::create_dir_all(&dest_run_dir)?;

    for name in ["transcript.jsonl", "timing.json"] {
        let source = source_run_dir.join(name);
        if source.is_file() {
            fs::copy(&source, dest_run_dir.join(name))?;
        }
    }

    let source_workspace = source_run_dir.join("workspace");
    let dest_workspace = dest_run_dir.join("workspace");
    if source_workspace.is_dir() {
        copy_dir_recursive(&source_workspace, &dest_workspace)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn sanitize_dir_name(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            slug.push(ch);
        } else {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        "eval".to_string()
    } else {
        slug
    }
}

fn sha256_digest(content: &str) -> String {
    sha256_digest_bytes(content.as_bytes())
}

fn sha256_digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", super::hex_encode(hasher.finalize()))
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    super::hex_encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{RunMetrics, RunPaths, ScenarioKind};
    use tempfile::tempdir;

    fn sample_key_input(scenario: ScenarioKind, skill_hash: &str, fixture_hash: &str) -> CacheKeyInput {
        sample_key_input_for_case("one", scenario, skill_hash, fixture_hash)
    }

    fn sample_key_input_for_case(
        eval_case_id: &str,
        scenario: ScenarioKind,
        skill_hash: &str,
        fixture_hash: &str,
    ) -> CacheKeyInput {
        CacheKeyInput {
            eval_case_id: eval_case_id.to_string(),
            skill_hash: skill_hash.to_string(),
            evals_hash: "sha256:evals".to_string(),
            fixture_hash: fixture_hash.to_string(),
            model_config: "ci-default".to_string(),
            runner_model: None,
            runner_kind: "codex".to_string(),
            runner_version: None,
            scenario,
            attempt: 1,
            prompt_contract_version: PROMPT_CONTRACT_VERSION.to_string(),
            environment: EnvironmentPolicy::default(),
            skill_staging: SkillStaging::default(),
            scaffold_hash: None,
        }
    }

    /// A run set up by another script worked in another directory, so it answers another
    /// question. `skill_hash` is the digest of `SKILL.md` alone and would not notice.
    #[test]
    fn a_run_from_another_scaffold_is_a_different_run() {
        let base = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
        let seeded = CacheKeyInput {
            scaffold_hash: Some("sha256:seeded".to_string()),
            ..base.clone()
        };
        let edited = CacheKeyInput {
            scaffold_hash: Some("sha256:edited".to_string()),
            ..base.clone()
        };

        assert_ne!(
            CacheKey::from_input(&seeded),
            CacheKey::from_input(&edited),
            "the state a run was handed belongs in its identity"
        );
        assert_ne!(
            CacheKey::from_input(&seeded),
            CacheKey::from_input(&base),
            "a run handed a scaffolded directory is not the run that got an empty one"
        );
        assert_ne!(
            ReuseKeyInput::of(&seeded),
            ReuseKeyInput::of(&edited),
            "--reuse-completed cannot forget it either"
        );
    }

    /// A case that declares no scaffold has to key exactly as it did before the field
    /// existed, or every entry already on disk is evicted for nothing.
    #[test]
    fn a_case_with_no_scaffold_keys_as_it_did_before() {
        let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
        let serialized = serde_json::to_string(&input).unwrap();
        assert!(
            !serialized.contains("scaffold_hash"),
            "an absent scaffold must not reach the key: {serialized}"
        );
    }

    /// A run that inherited this machine is not the run a scrubbed invocation asked
    /// for, so the completed one must not be served in its place.
    #[test]
    fn a_run_from_another_environment_policy_is_a_different_run() {
        let inherited = CacheKeyInput {
            environment: EnvironmentPolicy::Inherited,
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
        };
        let scrubbed = CacheKeyInput {
            environment: EnvironmentPolicy::Scrubbed,
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
        };
        let symlinked = CacheKeyInput {
            skill_staging: SkillStaging::Symlink,
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
        };

        assert_ne!(
            CacheKey::from_input(&inherited),
            CacheKey::from_input(&scrubbed),
            "the environment a run saw belongs in its identity"
        );
        assert_ne!(
            CacheKey::from_input(&symlinked),
            CacheKey::from_input(&sample_key_input(
                ScenarioKind::WithSkill,
                "sha256:skill",
                "sha256:fixture"
            )),
            "what the run could reach out of its workspace belongs in its identity"
        );
    }

    fn write_completed_run(report_dir: &Path, run_id: &str, eval_case_id: &str, scenario: ScenarioKind) {
        let run_dir = report_dir.join("runs").join(run_id);
        let workspace = run_dir.join("workspace");
        fs::create_dir_all(workspace.join("outputs")).unwrap();
        fs::write(run_dir.join("transcript.jsonl"), "{}\n").unwrap();
        fs::write(run_dir.join("timing.json"), r#"{"duration_ms":42}"#).unwrap();
        fs::write(workspace.join("outputs/final.md"), "done").unwrap();

        let run = RunRecord {
            id: run_id.to_string(),
            eval_case_id: eval_case_id.to_string(),
            eval_slug: eval_case_id.to_string(),
            scenario_id: scenario,
            iteration: 1,
            model_config_id: "ci-default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            failure_kind: None,
            runner_invocations: 1,
            status: "completed".to_string(),
            paths: RunPaths {
                workspace: format!("runs/{run_id}/workspace"),
                outputs: format!("runs/{run_id}/workspace/outputs"),
            },
            mirror_path: format!("iteration-1/eval-{eval_case_id}/{}/", scenario.as_str()),
            artifacts: vec![serde_json::json!({"kind":"transcript","path":format!("runs/{run_id}/transcript.jsonl")})],
            metrics: RunMetrics {
                duration_ms: Some(42),
                exit_code: Some(0),
                total_tokens: Some(10),
                input_tokens: Some(6),
                output_tokens: Some(4),
                cost_usd: None,
            },
            cache: None,
            skill_integrity: None,
            warnings: Vec::new(),
        };

        let document = serde_json::json!({
            "report": {"id":"r1","generated_at":"2026-01-01T00:00:00Z","iteration":1,"producer":{"name":"trg","version":"0.0.0"}},
            "suite": {"skill_name":"demo","skill_path":"demo","skill_hash":"sha256:skill","evals_path":"demo/evals/evals.json","evals_hash":"sha256:evals"},
            "dimensions": {"eval_cases":[],"assertions":[],"skill_revisions":[],"model_configs":[],"scenarios":[],"graders":[]},
            "runs": [run],
            "assertion_results": [],
            "summaries": {"by_scenario":[]},
            "comparisons": []
        });
        fs::write(
            report_dir.join("report.json"),
            serde_json::to_string_pretty(&document).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn cache_key_is_stable_for_identical_input() {
        let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures");
        let a = CacheKey::from_input(&input);
        let b = CacheKey::from_input(&input);
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_changes_when_scenario_differs() {
        let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures");
        let without_skill = sample_key_input(ScenarioKind::WithoutSkill, "sha256:skill", "sha256:fixtures");
        assert_ne!(CacheKey::from_input(&with_skill), CacheKey::from_input(&without_skill));
    }

    #[test]
    fn cache_key_changes_when_the_eval_case_differs() {
        let empty_fixtures = FixtureHash::empty();
        let first = sample_key_input_for_case("one", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
        let second = sample_key_input_for_case("two", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
        assert_ne!(CacheKey::from_input(&first), CacheKey::from_input(&second));
    }

    #[test]
    fn a_completed_run_is_not_served_to_a_different_eval_case() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let empty_fixtures = FixtureHash::empty();
        let first = sample_key_input_for_case("one", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
        record_completion(&out_dir, &CacheKey::from_input(&first), &first, &report_a, "run-001").unwrap();

        let second = sample_key_input_for_case("two", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
        let reuse_second = ReuseKeyInput::of(&second);
        let options = CacheOptions {
            enabled: true,
            reuse_completed: true,
        };

        assert!(try_resolve_cache(&out_dir, options, &second, &reuse_second).is_none());
        assert!(try_resolve_cache(&out_dir, options, &first, &reuse_second).is_some());
    }

    #[test]
    fn fixture_hash_is_stable_and_changes_with_content() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let fixtures = skill.join("evals/one/fixtures");
        fs::create_dir_all(&fixtures).unwrap();

        let first = compute_fixture_hash(&skill, "one").unwrap();
        fs::write(fixtures.join("input.txt"), "alpha").unwrap();
        let second = compute_fixture_hash(&skill, "one").unwrap();
        fs::write(fixtures.join("input.txt"), "beta").unwrap();
        let third = compute_fixture_hash(&skill, "one").unwrap();

        assert_eq!(first, FixtureHash::empty());
        assert_ne!(first, second);
        assert_ne!(second, third);
    }

    #[test]
    fn exact_cache_hit_reuses_completed_run() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        let key = CacheKey::from_input(&input);
        record_completion(&out_dir, &key, &input, &report_a, "run-001").unwrap();

        let report_b = out_dir.join("demo/report-b");
        fs::create_dir_all(report_b.join("runs/run-001/workspace/outputs")).unwrap();
        let mut run = RunRecord {
            id: "run-001".to_string(),
            eval_case_id: "one".to_string(),
            eval_slug: "one".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            iteration: 2,
            model_config_id: "ci-default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            failure_kind: None,
            runner_invocations: 0,
            status: "skipped".to_string(),
            paths: RunPaths {
                workspace: "runs/run-001/workspace".to_string(),
                outputs: "runs/run-001/workspace/outputs".to_string(),
            },
            mirror_path: "iteration-2/eval-one/with_skill/".to_string(),
            artifacts: Vec::new(),
            metrics: RunMetrics {
                duration_ms: None,
                exit_code: None,
                total_tokens: None,
                input_tokens: None,
                output_tokens: None,
                cost_usd: None,
            },
            cache: None,
            skill_integrity: None,
            warnings: Vec::new(),
        };

        let pointer = lookup_exact(&out_dir, &key, &input).unwrap();
        apply_cache_hit(&mut run, &key, &pointer, &report_b).unwrap();

        assert_eq!(run.status, "completed");
        assert_eq!(run.metrics.duration_ms, Some(42));
        assert!(run.cache.as_ref().unwrap().hit);
        assert_eq!(run.cache.as_ref().unwrap().source_run_id, "run-001");
        assert!(report_b.join("runs/run-001/transcript.jsonl").is_file());
        assert!(report_b.join("runs/run-001/workspace/outputs/final.md").is_file());
    }

    #[test]
    fn stale_skill_hash_invalidates_exact_cache_entry() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:old", FixtureHash::empty().as_str());
        let key = CacheKey::from_input(&stored);
        record_completion(&out_dir, &key, &stored, &report_a, "run-001").unwrap();

        let current = sample_key_input(ScenarioKind::WithSkill, "sha256:new", FixtureHash::empty().as_str());
        assert!(lookup_exact(&out_dir, &key, &current).is_none());
        assert!(!cache_root(&out_dir).join(key.as_str()).exists());
    }

    #[test]
    fn stale_fixture_hash_invalidates_reuse_entry() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures-old");
        let key = CacheKey::from_input(&stored);
        record_completion(&out_dir, &key, &stored, &report_a, "run-001").unwrap();

        let current = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures-new");
        assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&current)).is_none());
    }

    /// The two arms of a case differ in the scenario and nothing else, so a reuse that
    /// forgot it would answer the baseline with the with-skill run and report a delta of
    /// zero for a skill that was never exercised.
    #[test]
    fn reuse_completed_does_not_serve_one_arm_of_a_case_to_the_other() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        record_completion(
            &out_dir,
            &CacheKey::from_input(&with_skill),
            &with_skill,
            &report_a,
            "run-001",
        )
        .unwrap();

        let without_skill = sample_key_input(
            ScenarioKind::WithoutSkill,
            "sha256:skill",
            FixtureHash::empty().as_str(),
        );
        assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&without_skill)).is_none());

        let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&with_skill)).unwrap();
        assert_eq!(pointer.run_id, "run-001");
    }

    /// What `--reuse-completed` is for: the run that answered this case under a different
    /// model config is still the run that answered this case.
    #[test]
    fn reuse_completed_serves_a_run_produced_under_another_model_config() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        record_completion(&out_dir, &CacheKey::from_input(&stored), &stored, &report_a, "run-001").unwrap();

        let other_config = CacheKeyInput {
            model_config: "hand-tuned".to_string(),
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
        };
        assert!(lookup_exact(&out_dir, &CacheKey::from_input(&other_config), &other_config).is_none());

        let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&other_config)).unwrap();
        assert_eq!(pointer.run_id, "run-001");
    }

    /// A second attempt is a second sample of the same cell, which is the whole reason
    /// for asking for it. Serving it the first attempt's run turns the spread across
    /// attempts into zero and the mean into the first draw.
    #[test]
    fn a_second_draw_of_a_cell_is_a_different_run() {
        let first = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
        let second = CacheKeyInput {
            attempt: 2,
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
        };

        assert_ne!(
            CacheKey::from_input(&first),
            CacheKey::from_input(&second),
            "which draw of the cell a run is belongs in its identity"
        );
    }

    /// `--reuse-completed` forgets the model config and the runner. It cannot forget the
    /// draw, for the same reason the exact key cannot.
    #[test]
    fn reuse_completed_does_not_serve_one_attempt_to_the_next() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report_a = out_dir.join("demo/report-a");
        fs::create_dir_all(&report_a).unwrap();
        write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

        let first = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        record_completion(&out_dir, &CacheKey::from_input(&first), &first, &report_a, "run-001").unwrap();

        let second = CacheKeyInput {
            attempt: 2,
            ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
        };
        assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&second)).is_none());

        let report_b = out_dir.join("demo/report-b");
        fs::create_dir_all(&report_b).unwrap();
        write_completed_run(&report_b, "run-002", "one", ScenarioKind::WithSkill);
        record_completion(&out_dir, &CacheKey::from_input(&second), &second, &report_b, "run-002").unwrap();

        assert_eq!(
            lookup_reuse(&out_dir, &ReuseKeyInput::of(&first)).unwrap().run_id,
            "run-001",
            "one slot per draw, so a later draw does not evict an earlier one"
        );
        assert_eq!(
            lookup_reuse(&out_dir, &ReuseKeyInput::of(&second)).unwrap().run_id,
            "run-002"
        );
    }

    /// One slot per case would let each arm evict the other, so a comparison would
    /// re-execute every run it was told to reuse.
    #[test]
    fn each_arm_of_a_case_keeps_its_own_completed_run() {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");

        let with_report = out_dir.join("demo/report-with");
        fs::create_dir_all(&with_report).unwrap();
        write_completed_run(&with_report, "run-001", "one", ScenarioKind::WithSkill);
        let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        record_completion(
            &out_dir,
            &CacheKey::from_input(&with_skill),
            &with_skill,
            &with_report,
            "run-001",
        )
        .unwrap();

        let without_report = out_dir.join("demo/report-without");
        fs::create_dir_all(&without_report).unwrap();
        write_completed_run(&without_report, "run-002", "one", ScenarioKind::WithoutSkill);
        let without_skill = sample_key_input(
            ScenarioKind::WithoutSkill,
            "sha256:skill",
            FixtureHash::empty().as_str(),
        );
        record_completion(
            &out_dir,
            &CacheKey::from_input(&without_skill),
            &without_skill,
            &without_report,
            "run-002",
        )
        .unwrap();

        assert_eq!(
            lookup_reuse(&out_dir, &ReuseKeyInput::of(&with_skill)).unwrap().run_id,
            "run-001"
        );
        assert_eq!(
            lookup_reuse(&out_dir, &ReuseKeyInput::of(&without_skill))
                .unwrap()
                .run_id,
            "run-002"
        );
    }
}
