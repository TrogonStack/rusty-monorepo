mod constants;
use constants::{POINTER_FILE, REUSE_DIR};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::budget::RunCaps;
use super::evals::EvalDirName;
use super::mocks::MOCK_CALLS_LOG_NAME;
use super::report::{EnvironmentPolicy, PermissionGrant, RunRecord, ScenarioKind, SkillStaging};
use super::runner::Runner;
use super::tool_grant::ToolGrant;

pub use super::prompt::PROMPT_CONTRACT_VERSION;

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
    /// The resolved mock set's content hash, for a case that declares mcp mocks.
    ///
    /// `evals_hash` only covers mock content by accident, and only for a suite authored
    /// as case directories: `canonical_directory_hash` walks every file under a case
    /// directory, mocks included, but a manifest-authored suite's hash is the manifest
    /// bytes alone, and suite-level mocks under `evals/mocks/` sit outside any case
    /// directory either way. `None` for a case with no mocks rather than the empty
    /// set's hash, mirroring `scaffold_hash`: a case that declares none must serialize
    /// exactly as it did before this field existed, or every entry already on disk is
    /// evicted for nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_hash: Option<String>,
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
    /// What the run's harness was allowed to do without being asked.
    ///
    /// A grant decides what the harness could actually carry out, so a run answers only
    /// for the grant it ran under. Serving a workspace-write run to an invocation that
    /// asked for an unrestricted one would report a grant the run never had, which is the
    /// reproducibility the grant exists to provide reported away by the cache.
    #[serde(default)]
    pub permission: PermissionGrant,
    /// The tool grant the run's harness was actually given, once the operator's
    /// ceiling and the case's own declaration are combined.
    ///
    /// A different grant can permit a different set of tool calls, so a run answers
    /// only for the grant it ran under; serving it to a request under another grant
    /// would report tool access the run never had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_grant: Option<ToolGrant>,
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
    /// What the case's companion skills held, for a case that declares any.
    ///
    /// The distractors decide what the run had to choose between, and nothing else here
    /// notices one of them changing: `evals_hash` covers the bytes that name the path and
    /// `skill_hash` is the digest of the skill under test. A case that declares none
    /// serializes as it always did, which leaves entries written before this field readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_hash: Option<String>,
    /// What the run was allowed to spend in tool calls and turns.
    ///
    /// A cap decides whether a run was stopped as `budget_exhausted` or allowed to
    /// finish, so a run answers only for the caps it ran under. An unbounded run
    /// serializes as it did before this field existed, which keeps entries already on
    /// disk readable.
    #[serde(default, skip_serializing_if = "RunCaps::is_unbounded")]
    pub caps: RunCaps,
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
/// directory it was handed. Nor the companion skills, which are in that directory too and
/// are the very thing an unannounced case asks the run to choose between.
///
/// Nor the tool grant, for the same reason again. What a run could reach decides what it
/// could do, so serving a run made under one grant to a request made under another answers
/// a question nobody asked, and does it invisibly: the report would attribute the reused
/// run the grant of the request rather than the grant it actually ran with.
///
/// Nor the run caps. A run that made forty tool calls under no cap completed; the same run
/// under a cap of five would have been stopped and recorded as `budget_exhausted`, which
/// is the very outcome the cap was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReuseKeyInput {
    pub eval_case_id: String,
    pub skill_hash: String,
    pub evals_hash: String,
    pub fixture_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_hash: Option<String>,
    pub scenario: ScenarioKind,
    pub attempt: u32,
    #[serde(default)]
    pub scaffold_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_grant: Option<ToolGrant>,
    #[serde(default, skip_serializing_if = "RunCaps::is_unbounded")]
    pub caps: RunCaps,
}

impl ReuseKeyInput {
    pub fn of(key_input: &CacheKeyInput) -> Self {
        Self {
            eval_case_id: key_input.eval_case_id.clone(),
            skill_hash: key_input.skill_hash.clone(),
            evals_hash: key_input.evals_hash.clone(),
            mock_hash: key_input.mock_hash.clone(),
            fixture_hash: key_input.fixture_hash.clone(),
            scenario: key_input.scenario,
            attempt: key_input.attempt,
            scaffold_hash: key_input.scaffold_hash.clone(),
            companion_hash: key_input.companion_hash.clone(),
            tool_grant: key_input.tool_grant.clone(),
            caps: key_input.caps,
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

pub fn compute_fixture_hash(skill_path: &Path, eval_dir: &EvalDirName, eval_id: &str) -> io::Result<FixtureHash> {
    let fixtures_dir = skill_path.join(eval_dir.as_str()).join(eval_id).join("fixtures");
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
) -> io::Result<RestoredArtifacts> {
    let source_report = PathBuf::from(&pointer.report_dir);
    let source_run = load_source_run(&source_report, &pointer.run_id)?;
    let restored = copy_run_artifacts(&source_report, &pointer.run_id, report_dir, &run.id)?;

    // Named field by field, not `..source_run`, so a field added to `RunRecord` later
    // fails this match rather than silently keeping `run`'s own (usually empty) value.
    // Everything bound to an underscore-prefixed name belongs to the run doing the
    // reusing, not the run being reused: its own id, case, scenario, iteration,
    // attempt, invocation count, paths, mirror path and cache slot must stay as `run`
    // already has them.
    let RunRecord {
        id: _id,
        eval_case_id: _eval_case_id,
        eval_slug: _eval_slug,
        split: _split,
        scenario_id: _scenario_id,
        iteration: _iteration,
        model_config_id: _model_config_id,
        runner_model,
        runner_model_source,
        skill_revision_id: _skill_revision_id,
        attempt: _attempt,
        status,
        runner_invocations: _runner_invocations,
        failure_kind,
        paths: _paths,
        mirror_path: _mirror_path,
        tool_grant: _tool_grant,
        artifacts,
        metrics,
        cache: _cache,
        skill_integrity,
        read_only_fixture_violations,
        warnings,
        mock_violations,
        case_score: _case_score,
        trace: _trace,
        grade_trace: _grade_trace,
    } = source_run;

    run.status = status;
    // Taken from the source and not left as the request's own: `--reuse-completed` forgets
    // the model on purpose, so the artifacts below can have been produced under a different
    // one, and reporting the model that was asked for would name a model that never ran.
    run.runner_model = runner_model;
    run.runner_model_source = runner_model_source;
    run.metrics = metrics;
    run.artifacts = artifacts
        .into_iter()
        .map(|artifact| rewrite_artifact_run_id(artifact, &pointer.run_id, &run.id))
        .collect();
    run.skill_integrity = skill_integrity;
    run.failure_kind = failure_kind;
    run.read_only_fixture_violations = read_only_fixture_violations;
    run.warnings = warnings;
    run.mock_violations = mock_violations;
    run.cache = Some(RunCacheInfo {
        hit: true,
        source_run_id: pointer.run_id.clone(),
        key: key.as_str().to_string(),
    });
    Ok(restored)
}

/// How much a cache hit copied into the run reusing it, so the cost of a hit is visible
/// next to the cost of executing the run it replaced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoredArtifacts {
    files: u64,
    bytes: u64,
}

impl RestoredArtifacts {
    pub fn files(&self) -> u64 {
        self.files
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    fn add_file(&mut self, bytes: u64) {
        self.files += 1;
        self.bytes += bytes;
    }
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
) -> io::Result<RestoredArtifacts> {
    let source_run_dir = source_report.join("runs").join(source_run_id);
    let dest_run_dir = dest_report.join("runs").join(dest_run_id);
    fs::create_dir_all(&dest_run_dir)?;

    let mut restored = RestoredArtifacts::default();
    for name in ["transcript.jsonl", "timing.json", MOCK_CALLS_LOG_NAME] {
        let source = source_run_dir.join(name);
        if source.is_file() {
            restored.add_file(fs::copy(&source, dest_run_dir.join(name))?);
        }
    }

    let source_workspace = source_run_dir.join("workspace");
    let dest_workspace = dest_run_dir.join("workspace");
    if source_workspace.is_dir() {
        copy_dir_recursive(&source_workspace, &dest_workspace, &mut restored)?;
    }
    Ok(restored)
}

fn copy_dir_recursive(src: &Path, dest: &Path, restored: &mut RestoredArtifacts) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target, restored)?;
        } else if file_type.is_file() {
            restored.add_file(fs::copy(entry.path(), &target)?);
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
#[path = "cache_tests.rs"]
mod tests;
