use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evals::EvalError;
use super::report::ScenarioKind;

pub const RUBRIC_ITEMS: &[&str] = &[
    "organization",
    "formatting",
    "completeness",
    "usefulness",
    "polish",
    "domain_fit",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgeKind {
    #[default]
    None,
    Llm,
    Script,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioPair {
    pub a: ScenarioKind,
    pub b: ScenarioKind,
}

impl ScenarioPair {
    pub fn parse(raw: &str) -> Result<Self, EvalError> {
        let (left, right) = raw.split_once(':').ok_or_else(|| {
            EvalError::Validation(
                super::validation::ValidationError::for_field(
                    "pair",
                    format!("expected '<scenario>:<scenario>', got '{raw}'"),
                )
                .into(),
            )
        })?;

        let a = parse_scenario_kind(left.trim(), "pair")?;
        let b = parse_scenario_kind(right.trim(), "pair")?;
        if a == b {
            return Err(EvalError::Validation(
                super::validation::ValidationError::for_field("pair", format!("scenarios must differ, got '{raw}'"))
                    .into(),
            ));
        }

        Ok(Self { a, b })
    }
}

fn parse_scenario_kind(raw: &str, field: &str) -> Result<ScenarioKind, EvalError> {
    match raw {
        "with_skill" => Ok(ScenarioKind::WithSkill),
        "without_skill" => Ok(ScenarioKind::WithoutSkill),
        "old_skill" => Ok(ScenarioKind::OldSkill),
        _ => Err(EvalError::Validation(
            super::validation::ValidationError::for_field(
                field,
                format!("unknown scenario '{raw}' (expected with_skill, without_skill, or old_skill)"),
            )
            .into(),
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum BlindLabel {
    A,
    B,
}

impl BlindLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComparisonWinner {
    A,
    B,
    Tie,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScenarioPairRecord {
    pub a: ScenarioKind,
    pub b: ScenarioKind,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BlindLabelMapping {
    #[serde(rename = "A")]
    pub label_a: ScenarioKind,
    #[serde(rename = "B")]
    pub label_b: ScenarioKind,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ComparisonJudgeMetadata {
    pub kind: JudgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ComparisonRecord {
    pub eval_case_id: String,
    pub pair: ScenarioPairRecord,
    pub mapping: BlindLabelMapping,
    pub winner: ComparisonWinner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_scenario: Option<ScenarioKind>,
    pub evidence: String,
    pub rubric: Vec<String>,
    pub judge: ComparisonJudgeMetadata,
}

#[derive(Debug, Clone, Default)]
pub struct CompareOptions {
    pub pairs: Vec<ScenarioPair>,
    pub judge: JudgeKind,
    pub judge_model: Option<String>,
    pub judge_command: Option<String>,
    pub emit_comparison_json: bool,
}

#[derive(Debug, Deserialize)]
struct LoadedReport {
    dimensions: LoadedDimensions,
    runs: Vec<LoadedRun>,
    #[serde(default)]
    comparisons: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct LoadedDimensions {
    eval_cases: Vec<LoadedEvalCase>,
}

#[derive(Debug, Deserialize)]
struct LoadedEvalCase {
    id: String,
    prompt: String,
    expected_output: String,
}

#[derive(Debug, Deserialize)]
struct LoadedRun {
    eval_case_id: String,
    scenario_id: ScenarioKind,
    #[serde(default = "default_loaded_run_attempt")]
    attempt: u32,
    status: String,
    paths: LoadedRunPaths,
}

fn default_loaded_run_attempt() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct LoadedRunPaths {
    workspace: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct JudgeScriptInput {
    eval_case_id: String,
    prompt: String,
    expected_output: String,
    rubric: Vec<String>,
    outputs: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct JudgeScriptOutput {
    winner: String,
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct LlmJudgeResponse {
    winner: String,
    evidence: String,
}

pub fn run_comparisons(report_dir: &Path, options: CompareOptions) -> Result<Vec<ComparisonRecord>, EvalError> {
    if options.pairs.is_empty() || options.judge == JudgeKind::None {
        return Ok(Vec::new());
    }

    if options.judge == JudgeKind::Script && options.judge_command.as_deref().unwrap_or("").is_empty() {
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field("judge_command", "is required when --judge script").into(),
        ));
    }

    if options.judge == JudgeKind::Llm && options.judge_model.as_deref().unwrap_or("").is_empty() {
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field("judge_model", "is required when --judge llm").into(),
        ));
    }

    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", report_path.display()),
        ))
    })?;
    let mut report: LoadedReport = serde_json::from_str(&content)?;

    let mut records = Vec::new();
    for eval_case in &report.dimensions.eval_cases {
        for pair in &options.pairs {
            let left_output = load_scenario_output(report_dir, &report.runs, &eval_case.id, pair.a)?;
            let right_output = load_scenario_output(report_dir, &report.runs, &eval_case.id, pair.b)?;

            let (mapping, blind_outputs) = build_blind_pair(&eval_case.id, pair.a, pair.b, left_output, right_output);

            let verdict = match options.judge {
                JudgeKind::Script => {
                    let command = options.judge_command.as_deref().unwrap_or_default();
                    run_script_judge(
                        command,
                        &eval_case.id,
                        &eval_case.prompt,
                        &eval_case.expected_output,
                        &blind_outputs,
                    )?
                }
                JudgeKind::Llm => {
                    let model = options.judge_model.as_deref().unwrap_or_default();
                    run_llm_judge(
                        model,
                        &eval_case.id,
                        &eval_case.prompt,
                        &eval_case.expected_output,
                        &blind_outputs,
                    )?
                }
                JudgeKind::None => unreachable!(),
            };

            let winner_scenario = match verdict.winner {
                ComparisonWinner::A => Some(mapping.label_a),
                ComparisonWinner::B => Some(mapping.label_b),
                ComparisonWinner::Tie => None,
            };

            let record = ComparisonRecord {
                eval_case_id: eval_case.id.clone(),
                pair: ScenarioPairRecord { a: pair.a, b: pair.b },
                mapping,
                winner: verdict.winner,
                winner_scenario,
                evidence: verdict.evidence,
                rubric: RUBRIC_ITEMS.iter().map(|item| (*item).to_string()).collect(),
                judge: ComparisonJudgeMetadata {
                    kind: options.judge,
                    model: options.judge_model.clone(),
                    command: options.judge_command.clone(),
                },
            };

            if options.emit_comparison_json {
                write_comparison_json(report_dir, &record)?;
            }

            records.push(record);
        }
    }

    report.comparisons = records
        .iter()
        .map(|record| serde_json::to_value(record).expect("comparison record serializes"))
        .collect();

    merge_comparisons_into_report(&report_path, &content, &report.comparisons)?;

    Ok(records)
}

fn merge_comparisons_into_report(
    report_path: &Path,
    original_content: &str,
    comparisons: &[serde_json::Value],
) -> Result<(), EvalError> {
    let mut root: serde_json::Value = serde_json::from_str(original_content)?;
    let Some(object) = root.as_object_mut() else {
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field(
                format!("report '{}'", report_path.display()),
                "root must be a JSON object",
            )
            .into(),
        ));
    };
    object.insert(
        "comparisons".to_string(),
        serde_json::Value::Array(comparisons.to_vec()),
    );
    std::fs::write(report_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

struct Verdict {
    winner: ComparisonWinner,
    evidence: String,
}

pub fn build_blind_pair(
    eval_case_id: &str,
    scenario_a: ScenarioKind,
    scenario_b: ScenarioKind,
    output_a: String,
    output_b: String,
) -> (BlindLabelMapping, HashMap<String, String>) {
    let swap = shuffle_swap(eval_case_id);
    let mapping = if swap {
        BlindLabelMapping {
            label_a: scenario_b,
            label_b: scenario_a,
        }
    } else {
        BlindLabelMapping {
            label_a: scenario_a,
            label_b: scenario_b,
        }
    };

    let blind_outputs = if swap {
        HashMap::from([("A".to_string(), output_b), ("B".to_string(), output_a)])
    } else {
        HashMap::from([("A".to_string(), output_a), ("B".to_string(), output_b)])
    };

    (mapping, blind_outputs)
}

pub fn shuffle_swap(eval_case_id: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(eval_case_id.as_bytes());
    let digest = hasher.finalize();
    digest[0] % 2 == 1
}

pub fn blind_judge_payload(outputs: &HashMap<String, String>) -> serde_json::Value {
    serde_json::json!({
        "eval_case_id": "redacted",
        "prompt": "",
        "expected_output": "",
        "rubric": RUBRIC_ITEMS,
        "outputs": outputs,
    })
}

fn select_latest_completed_run<'a>(
    runs: &'a [LoadedRun],
    eval_case_id: &str,
    scenario: ScenarioKind,
) -> Option<&'a LoadedRun> {
    runs.iter()
        .filter(|run| run.eval_case_id == eval_case_id && run.scenario_id == scenario && run.status == "completed")
        .max_by_key(|run| run.attempt)
}

fn load_scenario_output(
    report_dir: &Path,
    runs: &[LoadedRun],
    eval_case_id: &str,
    scenario: ScenarioKind,
) -> Result<String, EvalError> {
    let run = select_latest_completed_run(runs, eval_case_id, scenario).ok_or_else(|| {
        EvalError::Validation(
            super::validation::ValidationError::for_field(
                "runs",
                format!(
                    "missing completed run for eval '{}' and scenario '{}'",
                    eval_case_id,
                    scenario.as_str()
                ),
            )
            .into(),
        )
    })?;

    let workspace = report_dir.join(&run.paths.workspace);
    collect_workspace_output(&workspace)
}

fn collect_workspace_output(workspace: &Path) -> Result<String, EvalError> {
    for candidate in ["output.md", "output.txt", "response.md", "response.txt"] {
        let path = workspace.join(candidate);
        if path.is_file() {
            return std::fs::read_to_string(&path).map_err(EvalError::from);
        }
    }

    if workspace.is_dir() {
        let mut chunks = Vec::new();
        collect_text_files(workspace, workspace, &mut chunks)?;
        if !chunks.is_empty() {
            chunks.sort_by(|left, right| left.0.cmp(&right.0));
            return Ok(chunks
                .into_iter()
                .map(|(path, content)| format!("## {path}\n{content}"))
                .collect::<Vec<_>>()
                .join("\n\n"));
        }
    }

    Err(EvalError::Validation(
        super::validation::ValidationError::for_field(
            "workspace",
            format!("no output found under '{}'", workspace.display()),
        )
        .into(),
    ))
}

fn collect_text_files(root: &Path, dir: &Path, matches: &mut Vec<(String, String)>) -> Result<(), EvalError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_text_files(root, &path, matches)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("md") | Some("txt") | Some("json")
        ) || name == "grading.json"
        {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            let content = std::fs::read_to_string(&path)?;
            if !content.trim().is_empty() {
                matches.push((relative, content));
            }
        }
    }

    Ok(())
}

fn run_script_judge(
    command: &str,
    eval_case_id: &str,
    prompt: &str,
    expected_output: &str,
    blind_outputs: &HashMap<String, String>,
) -> Result<Verdict, EvalError> {
    let payload = JudgeScriptInput {
        eval_case_id: eval_case_id.to_string(),
        prompt: prompt.to_string(),
        expected_output: expected_output.to_string(),
        rubric: RUBRIC_ITEMS.iter().map(|item| (*item).to_string()).collect(),
        outputs: blind_outputs.clone(),
    };

    let input = serde_json::to_string(&payload)?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(EvalError::from)?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(input.as_bytes()).map_err(EvalError::from)?;
    }

    let output = child.wait_with_output().map_err(EvalError::from)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field("judge_command", format!("judge script failed: {stderr}"))
                .into(),
        ));
    }

    let parsed: JudgeScriptOutput = serde_json::from_slice(&output.stdout).map_err(|source| {
        EvalError::Validation(
            super::validation::ValidationError::for_field(
                "judge_command",
                format!("judge script returned invalid JSON: {source}"),
            )
            .into(),
        )
    })?;

    Ok(Verdict {
        winner: parse_winner(&parsed.winner)?,
        evidence: parsed.evidence,
    })
}

fn run_llm_judge(
    model: &str,
    eval_case_id: &str,
    prompt: &str,
    expected_output: &str,
    blind_outputs: &HashMap<String, String>,
) -> Result<Verdict, EvalError> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        EvalError::Validation(
            super::validation::ValidationError::for_field("judge_model", "OPENAI_API_KEY must be set for llm judging")
                .into(),
        )
    })?;

    let system_prompt = format!(
        "You are a blind evaluator comparing two anonymous outputs labeled A and B. \
         Judge only the provided rubric dimensions: {}. \
         Respond with JSON: {{\"winner\":\"A|B|tie\",\"evidence\":\"...\"}}. \
         Do not infer scenario identity.",
        RUBRIC_ITEMS.join(", ")
    );
    let user_prompt = serde_json::to_string(&JudgeScriptInput {
        eval_case_id: eval_case_id.to_string(),
        prompt: prompt.to_string(),
        expected_output: expected_output.to_string(),
        rubric: RUBRIC_ITEMS.iter().map(|item| (*item).to_string()).collect(),
        outputs: blind_outputs.clone(),
    })?;

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "response_format": {"type": "json_object"}
    });

    let response = reqwest::blocking::Client::new()
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|source| {
            EvalError::Validation(
                super::validation::ValidationError::for_field("judge_model", source.to_string()).into(),
            )
        })?;

    if !response.status().is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field("judge_model", format!("llm judge request failed: {detail}"))
                .into(),
        ));
    }

    let payload: serde_json::Value = response.json().map_err(|source| {
        EvalError::Validation(super::validation::ValidationError::for_field("judge_model", source.to_string()).into())
    })?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            EvalError::Validation(
                super::validation::ValidationError::for_field("judge_model", "llm judge returned no message content")
                    .into(),
            )
        })?;

    let parsed: LlmJudgeResponse = serde_json::from_str(content).map_err(|source| {
        EvalError::Validation(
            super::validation::ValidationError::for_field(
                "judge_model",
                format!("llm judge returned invalid JSON: {source}"),
            )
            .into(),
        )
    })?;

    Ok(Verdict {
        winner: parse_winner(&parsed.winner)?,
        evidence: parsed.evidence,
    })
}

fn parse_winner(raw: &str) -> Result<ComparisonWinner, EvalError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "a" => Ok(ComparisonWinner::A),
        "b" => Ok(ComparisonWinner::B),
        "tie" => Ok(ComparisonWinner::Tie),
        _ => Err(EvalError::Validation(
            super::validation::ValidationError::for_field("winner", format!("expected A, B, or tie, got '{raw}'"))
                .into(),
        )),
    }
}

fn write_comparison_json(report_dir: &Path, record: &ComparisonRecord) -> Result<(), EvalError> {
    if let Some(eval_dir) = find_iteration_eval_dir(report_dir, &record.eval_case_id) {
        let path = eval_dir.join("comparison.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(record)?)?;
    }
    Ok(())
}

fn find_iteration_eval_dir(report_dir: &Path, eval_case_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(report_dir).ok()?;
    let mut latest: Option<(u32, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(iteration) = parse_iteration_dir_number(&name) else {
            continue;
        };
        let eval_dir = path.join(eval_case_id);
        if !eval_dir.is_dir() {
            continue;
        }
        if latest.as_ref().is_none_or(|(best, _)| iteration > *best) {
            latest = Some((iteration, eval_dir));
        }
    }

    latest.map(|(_, eval_dir)| eval_dir)
}

fn parse_iteration_dir_number(name: &str) -> Option<u32> {
    name.strip_prefix("iteration-")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use std::path::Path;

    fn sample_skill(fs: &MemFS) -> PathBuf {
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
                    {
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "assertions": ["assert a"]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "assertions": ["assert b"]
                    }
                ]
            }"#,
        );
        skill_path.to_path_buf()
    }

    fn write_workspace_output(report_dir: &Path, workspace: &str, content: &str) {
        let path = report_dir.join(workspace).join("output.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn mark_all_runs_completed(report_dir: &Path) {
        let report_path = report_dir.join("report.json");
        let mut report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
        for run in report["runs"].as_array_mut().unwrap() {
            run["status"] = serde_json::json!("completed");
        }
        std::fs::write(report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }

    #[test]
    fn load_scenario_output_prefers_latest_completed_attempt() {
        let temp = tempfile::tempdir().unwrap();
        let report = serde_json::json!({
            "runs": [
                {
                    "eval_case_id": "case-a",
                    "scenario_id": "with_skill",
                    "attempt": 1,
                    "status": "failed",
                    "paths": { "workspace": "runs/run-001/workspace" }
                },
                {
                    "eval_case_id": "case-a",
                    "scenario_id": "with_skill",
                    "attempt": 2,
                    "status": "completed",
                    "paths": { "workspace": "runs/run-002/workspace" }
                },
                {
                    "eval_case_id": "case-a",
                    "scenario_id": "with_skill",
                    "attempt": 3,
                    "status": "skipped",
                    "paths": { "workspace": "runs/run-003/workspace" }
                }
            ]
        });
        let runs: Vec<LoadedRun> = serde_json::from_value(report["runs"].clone()).unwrap();

        write_workspace_output(temp.path(), "runs/run-001/workspace", "attempt-1");
        write_workspace_output(temp.path(), "runs/run-002/workspace", "attempt-2");
        write_workspace_output(temp.path(), "runs/run-003/workspace", "attempt-3");

        let output = load_scenario_output(temp.path(), &runs, "case-a", ScenarioKind::WithSkill).unwrap();
        assert_eq!(output, "attempt-2");
    }

    #[test]
    fn find_iteration_eval_dir_prefers_latest_iteration() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("iteration-1/case-a")).unwrap();
        std::fs::create_dir_all(temp.path().join("iteration-2/case-a")).unwrap();
        std::fs::create_dir_all(temp.path().join("iteration-3/case-b")).unwrap();

        assert_eq!(
            find_iteration_eval_dir(temp.path(), "case-a"),
            Some(temp.path().join("iteration-2/case-a"))
        );
        assert_eq!(
            find_iteration_eval_dir(temp.path(), "case-b"),
            Some(temp.path().join("iteration-3/case-b"))
        );
        assert_eq!(find_iteration_eval_dir(temp.path(), "missing"), None);
    }

    #[test]
    fn merge_comparisons_into_report_rejects_non_object_root() {
        let temp = tempfile::tempdir().unwrap();
        let report_path = temp.path().join("report.json");
        std::fs::write(&report_path, "[]").unwrap();

        let error = merge_comparisons_into_report(&report_path, "[]", &[]).unwrap_err();
        assert!(error.to_string().contains("root must be a JSON object"));
    }

    #[test]
    fn shuffle_swap_is_deterministic_per_eval_id() {
        assert_eq!(shuffle_swap("case-a"), shuffle_swap("case-a"));
        assert_eq!(shuffle_swap("case-b"), shuffle_swap("case-b"));
    }

    #[test]
    fn shuffle_swap_varies_by_eval_id() {
        let values: Vec<bool> = ["case-a", "case-b", "case-c", "case-d"]
            .into_iter()
            .map(shuffle_swap)
            .collect();
        assert!(values.contains(&true) || values.contains(&false));
    }

    #[test]
    fn build_blind_pair_hides_scenario_identity() {
        let (mapping, outputs) = build_blind_pair(
            "case-a",
            ScenarioKind::WithSkill,
            ScenarioKind::WithoutSkill,
            "with-skill-output".to_string(),
            "without-skill-output".to_string(),
        );

        let payload = blind_judge_payload(&outputs);
        let serialized = payload.to_string();
        assert!(!serialized.contains("with_skill"));
        assert!(!serialized.contains("without_skill"));
        assert!(outputs.contains_key("A"));
        assert!(outputs.contains_key("B"));

        if mapping.label_a == ScenarioKind::WithSkill {
            assert_eq!(outputs.get("A").unwrap(), "with-skill-output");
            assert_eq!(outputs.get("B").unwrap(), "without-skill-output");
        } else {
            assert_eq!(outputs.get("A").unwrap(), "without-skill-output");
            assert_eq!(outputs.get("B").unwrap(), "with-skill-output");
        }
    }

    #[test]
    fn run_comparisons_with_script_judge_updates_report_and_iteration_json() {
        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = sample_skill(&fs);
        let bundle = build_report_bundle(
            &fs,
            &skill_path,
            &skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions::default(),
        )
        .unwrap();
        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

        for run in &bundle.document.runs {
            write_workspace_output(
                &report_dir,
                &run.paths.workspace,
                &format!("{}-output", run.scenario_id.as_str()),
            );
        }
        mark_all_runs_completed(&report_dir);

        let iteration_dir = report_dir.join("iteration-1").join("case-a");
        std::fs::create_dir_all(&iteration_dir).unwrap();

        let judge_script = temp.path().join("judge.py");
        std::fs::write(
            &judge_script,
            r#"import json, sys
payload = json.load(sys.stdin)
print(json.dumps({"winner": "A", "evidence": "A is clearer"}))
"#,
        )
        .unwrap();

        let records = run_comparisons(
            &report_dir,
            CompareOptions {
                pairs: vec![ScenarioPair {
                    a: ScenarioKind::WithSkill,
                    b: ScenarioKind::WithoutSkill,
                }],
                judge: JudgeKind::Script,
                judge_model: None,
                judge_command: Some(format!("python3 {}", judge_script.display())),
                emit_comparison_json: true,
            },
        )
        .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].eval_case_id, "case-a");
        assert_eq!(records[0].winner, ComparisonWinner::A);
        assert_eq!(records[0].evidence, "A is clearer");

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report.get("comparisons").and_then(|v| v.as_array()).unwrap().len(), 2);

        let comparison_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(iteration_dir.join("comparison.json")).unwrap()).unwrap();
        assert_eq!(
            comparison_json.get("eval_case_id").and_then(|v| v.as_str()),
            Some("case-a")
        );
    }

    #[test]
    fn run_comparisons_skips_when_judge_is_none() {
        let temp = tempfile::tempdir().unwrap();
        let fs = MemFS::new();
        let skill_path = sample_skill(&fs);
        let bundle = build_report_bundle(
            &fs,
            &skill_path,
            &skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions::default(),
        )
        .unwrap();
        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

        let records = run_comparisons(
            &report_dir,
            CompareOptions {
                pairs: vec![ScenarioPair {
                    a: ScenarioKind::WithSkill,
                    b: ScenarioKind::WithoutSkill,
                }],
                judge: JudgeKind::None,
                judge_model: None,
                judge_command: None,
                emit_comparison_json: false,
            },
        )
        .unwrap();

        assert!(records.is_empty());
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report.get("comparisons").and_then(|v| v.as_array()).unwrap().len(), 0);
    }
}
