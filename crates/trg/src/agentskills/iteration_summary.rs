use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::benchmark::{FailedRunsMode, stddev};
use super::evals::{EvalError, Result};
use super::layout;
use super::report::ScenarioKind;

pub const SCHEMA_VERSION: &str = "trg.skills-eval.iteration-summary.v1";
pub const OUTPUT_FILE_NAME: &str = "iteration-summary.json";

#[derive(Debug, Clone, Default)]
pub struct IterationSummaryOptions {
    pub failed_runs: FailedRunsMode,
    pub previous_report_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationSummaryDocument {
    pub schema_version: String,
    pub report_id: String,
    pub iteration: u32,
    pub generated_at: String,
    pub failed_runs_mode: FailedRunsMode,
    pub always_pass: Vec<AssertionStabilityRecord>,
    pub always_fail: Vec<AssertionStabilityRecord>,
    pub helped_by_skill: Vec<HelpedBySkillRecord>,
    pub flaky_assertions: Vec<FlakyAssertionRecord>,
    pub timing_outliers: Vec<TimingOutlierRecord>,
    pub token_outliers: Vec<TokenOutlierRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_iteration: Option<CrossIterationSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AssertionKey {
    pub eval_id: String,
    pub assertion_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionStabilityRecord {
    pub eval_id: String,
    pub assertion_text: String,
    pub attempts_observed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_iteration_delta: Option<CrossIterationDelta>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrossIterationDelta {
    New,
    Stable,
    Lost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelpedBySkillRecord {
    pub eval_id: String,
    pub assertion_text: String,
    pub with_skill_pass_rate: f64,
    pub without_skill_pass_rate: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlakyAssertionRecord {
    pub eval_id: String,
    pub scenario: String,
    pub assertion_text: String,
    pub attempts: u32,
    pub pass_count: u32,
    pub flakiness_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingOutlierRecord {
    pub eval_id: String,
    pub scenario: String,
    pub attempt: u32,
    pub duration_ms: u64,
    pub mean_ms: f64,
    pub stddev_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenOutlierRecord {
    pub eval_id: String,
    pub scenario: String,
    pub attempt: u32,
    pub total_tokens: u64,
    pub mean_ms: f64,
    pub stddev_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossIterationSection {
    pub previous_report_id: String,
    pub previous_iteration: u32,
    pub newly_always_pass: Vec<AssertionStabilityRecord>,
    pub no_longer_always_pass: Vec<AssertionStabilityRecord>,
    pub newly_always_fail: Vec<AssertionStabilityRecord>,
    pub no_longer_always_fail: Vec<AssertionStabilityRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportForSummary {
    report: ReportMeta,
    runs: Vec<RunForSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportMeta {
    id: String,
    iteration: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct RunForSummary {
    eval_case_id: String,
    scenario_id: ScenarioKind,
    attempt: u32,
    status: String,
    paths: RunPathsForSummary,
}

#[derive(Debug, Clone, Deserialize)]
struct RunPathsForSummary {
    workspace: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GradingFileInput {
    #[serde(default)]
    assertion_results: Vec<AssertionResultInput>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AssertionResultInput {
    #[serde(default, alias = "text")]
    assertion: String,
    #[serde(default)]
    passed: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TimingFileInput {
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct RunSample {
    grading: Option<GradingFileInput>,
    timing: Option<TimingFileInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AssertionObservationKey {
    eval_id: String,
    assertion_text: String,
}

impl AssertionObservationKey {
    fn new(eval_id: &str, assertion_text: &str) -> Self {
        Self {
            eval_id: eval_id.to_string(),
            assertion_text: normalize_assertion_key(assertion_text),
        }
    }
}

#[derive(Debug, Default)]
struct ScenarioMetricSample {
    eval_id: String,
    attempt: u32,
    value: u64,
}

pub fn build_iteration_summary_document(
    report_dir: &Path,
    options: IterationSummaryOptions,
) -> Result<IterationSummaryDocument> {
    let report = load_report(report_dir)?;
    let current = analyze_report(report_dir, &report, options.failed_runs);

    let previous_report_dir = options
        .previous_report_dir
        .clone()
        .or_else(|| detect_previous_report_dir(report_dir, report.report.iteration));

    let cross_iteration = previous_report_dir
        .as_ref()
        .and_then(|previous_dir| build_cross_iteration_section(previous_dir, &current, options.failed_runs));

    let (always_pass, always_fail) = apply_cross_iteration_deltas(
        &current.always_pass,
        &current.always_fail,
        cross_iteration.as_ref(),
    );

    Ok(IterationSummaryDocument {
        schema_version: SCHEMA_VERSION.to_string(),
        report_id: report.report.id,
        iteration: report.report.iteration,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        failed_runs_mode: options.failed_runs,
        always_pass,
        always_fail,
        helped_by_skill: current.helped_by_skill,
        flaky_assertions: current.flaky_assertions,
        timing_outliers: current.timing_outliers,
        token_outliers: current.token_outliers,
        cross_iteration,
    })
}

pub fn write_iteration_summary(report_dir: &Path, document: &IterationSummaryDocument) -> Result<PathBuf> {
    let output_path = report_dir.join(OUTPUT_FILE_NAME);
    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(&output_path, &json)?;

    let iteration_dir = report_dir.join(layout::iteration_dir_name(document.iteration));
    std::fs::create_dir_all(&iteration_dir)?;
    std::fs::write(iteration_dir.join(OUTPUT_FILE_NAME), json)?;

    Ok(output_path)
}

pub fn print_human_summary(document: &IterationSummaryDocument) {
    print_section(
        "Always pass",
        document.always_pass.iter().map(|record| {
            format!(
                "{} | {} ({} attempts{})",
                record.eval_id,
                record.assertion_text,
                record.attempts_observed,
                delta_suffix(record.cross_iteration_delta)
            )
        }),
    );
    print_section(
        "Always fail",
        document.always_fail.iter().map(|record| {
            format!(
                "{} | {} ({} attempts{})",
                record.eval_id,
                record.assertion_text,
                record.attempts_observed,
                delta_suffix(record.cross_iteration_delta)
            )
        }),
    );
    print_section(
        "Helped by skill",
        document.helped_by_skill.iter().map(|record| {
            format!(
                "{} | {} (with {:.0}% vs without {:.0}%, delta {:+.0}%)",
                record.eval_id,
                record.assertion_text,
                record.with_skill_pass_rate * 100.0,
                record.without_skill_pass_rate * 100.0,
                record.delta * 100.0
            )
        }),
    );
    print_section(
        "Flaky across attempts",
        document.flaky_assertions.iter().map(|record| {
            format!(
                "{} | {} | {} ({}/{} passes, {:.0}% flaky)",
                record.eval_id,
                record.scenario,
                record.assertion_text,
                record.pass_count,
                record.attempts,
                record.flakiness_ratio * 100.0
            )
        }),
    );
    print_section(
        "Timing outliers",
        document.timing_outliers.iter().map(|record| {
            format!(
                "{} | {} attempt {} | {} ms (mean {:.0}, σ {:.0})",
                record.eval_id,
                record.scenario,
                record.attempt,
                record.duration_ms,
                record.mean_ms,
                record.stddev_ms
            )
        }),
    );
    print_section(
        "Token outliers",
        document.token_outliers.iter().map(|record| {
            format!(
                "{} | {} attempt {} | {} tokens (mean {:.0}, σ {:.0})",
                record.eval_id,
                record.scenario,
                record.attempt,
                record.total_tokens,
                record.mean_ms,
                record.stddev_ms
            )
        }),
    );

    if let Some(cross) = &document.cross_iteration {
        println!();
        println!(
            "Cross-iteration vs {} (iteration {}):",
            cross.previous_report_id, cross.previous_iteration
        );
        print_section(
            "Newly always pass",
            cross.newly_always_pass.iter().map(|record| {
                format!("{} | {}", record.eval_id, record.assertion_text)
            }),
        );
        print_section(
            "No longer always pass",
            cross.no_longer_always_pass.iter().map(|record| {
                format!("{} | {}", record.eval_id, record.assertion_text)
            }),
        );
        print_section(
            "Newly always fail",
            cross.newly_always_fail.iter().map(|record| {
                format!("{} | {}", record.eval_id, record.assertion_text)
            }),
        );
        print_section(
            "No longer always fail",
            cross.no_longer_always_fail.iter().map(|record| {
                format!("{} | {}", record.eval_id, record.assertion_text)
            }),
        );
    }
}

fn delta_suffix(delta: Option<CrossIterationDelta>) -> String {
    match delta {
        Some(CrossIterationDelta::New) => " [new]".to_string(),
        Some(CrossIterationDelta::Stable) => " [stable]".to_string(),
        Some(CrossIterationDelta::Lost) => " [lost]".to_string(),
        None => String::new(),
    }
}

fn print_section<I>(title: &str, rows: I)
where
    I: IntoIterator<Item = String>,
{
    let rows: Vec<_> = rows.into_iter().collect();
    println!();
    println!("{title} ({}):", rows.len());
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    for row in rows {
        println!("  {row}");
    }
}

#[derive(Debug, Default)]
struct AnalysisResult {
    always_pass: Vec<AssertionStabilityRecord>,
    always_fail: Vec<AssertionStabilityRecord>,
    helped_by_skill: Vec<HelpedBySkillRecord>,
    flaky_assertions: Vec<FlakyAssertionRecord>,
    timing_outliers: Vec<TimingOutlierRecord>,
    token_outliers: Vec<TokenOutlierRecord>,
}

fn analyze_report(report_dir: &Path, report: &ReportForSummary, mode: FailedRunsMode) -> AnalysisResult {
    let mut assertion_outcomes: BTreeMap<AssertionObservationKey, (u32, u32)> = BTreeMap::new();
    let mut scenario_assertion_rates: BTreeMap<(String, ScenarioKind, String), (u32, u32)> = BTreeMap::new();
    let mut flaky_groups: BTreeMap<(String, ScenarioKind, String), BTreeMap<u32, bool>> = BTreeMap::new();
    let mut duration_by_scenario: HashMap<ScenarioKind, Vec<ScenarioMetricSample>> = HashMap::new();
    let mut tokens_by_scenario: HashMap<ScenarioKind, Vec<ScenarioMetricSample>> = HashMap::new();

    for run in &report.runs {
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }

        let sample = load_run_sample(report_dir, &run.paths.workspace);

        if let Some(grading) = &sample.grading {
            for result in &grading.assertion_results {
                let assertion_text = normalize_assertion_key(&result.assertion);
                if assertion_text.is_empty() {
                    continue;
                }

                let key = AssertionObservationKey::new(&run.eval_case_id, &assertion_text);
                let entry = assertion_outcomes.entry(key.clone()).or_insert((0, 0));
                if result.passed {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }

                let scenario_entry = scenario_assertion_rates
                    .entry((run.eval_case_id.clone(), run.scenario_id, assertion_text.clone()))
                    .or_insert((0, 0));
                if result.passed {
                    scenario_entry.0 += 1;
                } else {
                    scenario_entry.1 += 1;
                }

                flaky_groups
                    .entry((run.eval_case_id.clone(), run.scenario_id, assertion_text))
                    .or_default()
                    .insert(run.attempt, result.passed);
            }
        }

        if let Some(timing) = &sample.timing {
            if let Some(duration_ms) = timing.duration_ms {
                duration_by_scenario
                    .entry(run.scenario_id)
                    .or_default()
                    .push(ScenarioMetricSample {
                        eval_id: run.eval_case_id.clone(),
                        attempt: run.attempt,
                        value: duration_ms,
                    });
            }
            if let Some(total_tokens) = timing.total_tokens {
                tokens_by_scenario
                    .entry(run.scenario_id)
                    .or_default()
                    .push(ScenarioMetricSample {
                        eval_id: run.eval_case_id.clone(),
                        attempt: run.attempt,
                        value: total_tokens,
                    });
            }
        }
    }

    let always_pass = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *passed > 0 && *failed == 0)
        .map(|(key, (passed, _))| AssertionStabilityRecord {
            eval_id: key.eval_id.clone(),
            assertion_text: key.assertion_text.clone(),
            attempts_observed: *passed,
            cross_iteration_delta: None,
        })
        .collect();

    let always_fail = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *failed > 0 && *passed == 0)
        .map(|(key, (_, failed))| AssertionStabilityRecord {
            eval_id: key.eval_id.clone(),
            assertion_text: key.assertion_text.clone(),
            attempts_observed: *failed,
            cross_iteration_delta: None,
        })
        .collect();

    let helped_by_skill = scenario_assertion_rates
        .iter()
        .filter_map(|((eval_id, scenario, assertion_text), (passed, failed))| {
            if *scenario != ScenarioKind::WithSkill {
                return None;
            }
            let with_rate = pass_rate(*passed, *failed);
            let (without_passed, without_failed) = scenario_assertion_rates
                .get(&(eval_id.clone(), ScenarioKind::WithoutSkill, assertion_text.clone()))
                .copied()
                .unwrap_or((0, 0));
            let without_rate = pass_rate(without_passed, without_failed);
            if with_rate <= without_rate {
                return None;
            }
            Some(HelpedBySkillRecord {
                eval_id: eval_id.clone(),
                assertion_text: assertion_text.clone(),
                with_skill_pass_rate: with_rate,
                without_skill_pass_rate: without_rate,
                delta: with_rate - without_rate,
            })
        })
        .collect();

    let flaky_assertions = flaky_groups
        .into_iter()
        .filter_map(|((eval_id, scenario, assertion_text), attempts)| {
            let pass_count = attempts.values().filter(|passed| **passed).count() as u32;
            let total = attempts.len() as u32;
            if total <= 1 || pass_count == 0 || pass_count == total {
                return None;
            }
            let fail_count = total - pass_count;
            let flakiness_ratio = fail_count as f64 / total as f64;
            Some(FlakyAssertionRecord {
                eval_id,
                scenario: scenario.as_str().to_string(),
                assertion_text,
                attempts: total,
                pass_count,
                flakiness_ratio,
            })
        })
        .collect();

    AnalysisResult {
        always_pass,
        always_fail,
        helped_by_skill,
        flaky_assertions,
        timing_outliers: detect_timing_outliers(duration_by_scenario),
        token_outliers: detect_token_outliers(tokens_by_scenario),
    }
}

fn apply_cross_iteration_deltas(
    always_pass: &[AssertionStabilityRecord],
    always_fail: &[AssertionStabilityRecord],
    cross_iteration: Option<&CrossIterationSection>,
) -> (Vec<AssertionStabilityRecord>, Vec<AssertionStabilityRecord>) {
    let Some(cross) = cross_iteration else {
        return (always_pass.to_vec(), always_fail.to_vec());
    };

    let newly_pass: HashSet<_> = cross
        .newly_always_pass
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_id.clone(),
            assertion_text: record.assertion_text.clone(),
        })
        .collect();
    let lost_pass: HashSet<_> = cross
        .no_longer_always_pass
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_id.clone(),
            assertion_text: record.assertion_text.clone(),
        })
        .collect();
    let newly_fail: HashSet<_> = cross
        .newly_always_fail
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_id.clone(),
            assertion_text: record.assertion_text.clone(),
        })
        .collect();
    let lost_fail: HashSet<_> = cross
        .no_longer_always_fail
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_id.clone(),
            assertion_text: record.assertion_text.clone(),
        })
        .collect();

    let always_pass = always_pass
        .iter()
        .map(|record| {
            let key = AssertionKey {
                eval_id: record.eval_id.clone(),
                assertion_text: record.assertion_text.clone(),
            };
            let cross_iteration_delta = if newly_pass.contains(&key) {
                Some(CrossIterationDelta::New)
            } else if lost_pass.contains(&key) {
                Some(CrossIterationDelta::Lost)
            } else {
                Some(CrossIterationDelta::Stable)
            };
            AssertionStabilityRecord {
                cross_iteration_delta,
                ..record.clone()
            }
        })
        .collect();

    let always_fail = always_fail
        .iter()
        .map(|record| {
            let key = AssertionKey {
                eval_id: record.eval_id.clone(),
                assertion_text: record.assertion_text.clone(),
            };
            let cross_iteration_delta = if newly_fail.contains(&key) {
                Some(CrossIterationDelta::New)
            } else if lost_fail.contains(&key) {
                Some(CrossIterationDelta::Lost)
            } else {
                Some(CrossIterationDelta::Stable)
            };
            AssertionStabilityRecord {
                cross_iteration_delta,
                ..record.clone()
            }
        })
        .collect();

    (always_pass, always_fail)
}

fn build_cross_iteration_section(
    previous_dir: &Path,
    current: &AnalysisResult,
    mode: FailedRunsMode,
) -> Option<CrossIterationSection> {
    let previous_report = load_report(previous_dir).ok()?;
    let previous = analyze_report(previous_dir, &previous_report, mode);

    let current_pass = stability_key_set(&current.always_pass);
    let previous_pass = stability_key_set(&previous.always_pass);
    let current_fail = stability_key_set(&current.always_fail);
    let previous_fail = stability_key_set(&previous.always_fail);

    Some(CrossIterationSection {
        previous_report_id: previous_report.report.id,
        previous_iteration: previous_report.report.iteration,
        newly_always_pass: diff_records(&current.always_pass, &previous_pass),
        no_longer_always_pass: diff_records(&previous.always_pass, &current_pass),
        newly_always_fail: diff_records(&current.always_fail, &previous_fail),
        no_longer_always_fail: diff_records(&previous.always_fail, &current_fail),
    })
}

fn stability_key_set(records: &[AssertionStabilityRecord]) -> HashSet<AssertionKey> {
    records
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_id.clone(),
            assertion_text: record.assertion_text.clone(),
        })
        .collect()
}

fn diff_records(records: &[AssertionStabilityRecord], exclude: &HashSet<AssertionKey>) -> Vec<AssertionStabilityRecord> {
    records
        .iter()
        .filter(|record| {
            !exclude.contains(&AssertionKey {
                eval_id: record.eval_id.clone(),
                assertion_text: record.assertion_text.clone(),
            })
        })
        .cloned()
        .map(|mut record| {
            record.cross_iteration_delta = None;
            record
        })
        .collect()
}

pub fn detect_previous_report_dir(report_dir: &Path, current_iteration: u32) -> Option<PathBuf> {
    if current_iteration <= 1 {
        return None;
    }

    let target_iteration = current_iteration - 1;
    let skill_root = report_dir.parent()?;

    let entries = std::fs::read_dir(skill_root).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path();
        if !candidate.is_dir() || candidate == report_dir {
            continue;
        }
        if candidate.join("report.json").is_file() {
            if let Ok(report) = load_report(&candidate) {
                if report.report.iteration == target_iteration {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn load_report(report_dir: &Path) -> Result<ReportForSummary> {
    let content = std::fs::read_to_string(report_dir.join("report.json"))?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

enum RunDisposition {
    Completed,
    Failed,
    Skipped,
    Excluded,
}

fn classify_run(run: &RunForSummary, mode: FailedRunsMode) -> RunDisposition {
    match run.status.as_str() {
        "skipped" => RunDisposition::Skipped,
        "failed" => match mode {
            FailedRunsMode::Exclude => RunDisposition::Excluded,
            FailedRunsMode::Zero | FailedRunsMode::Bucket => RunDisposition::Failed,
        },
        _ => RunDisposition::Completed,
    }
}

fn load_run_sample(report_dir: &Path, workspace_rel: &str) -> RunSample {
    let workspace_dir = report_dir.join(workspace_rel);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());

    let grading_path = [run_dir.join("grading.json"), workspace_dir.join("grading.json")]
        .into_iter()
        .find(|path| path.is_file());
    let timing_path = [run_dir.join("timing.json"), workspace_dir.join("timing.json")]
        .into_iter()
        .find(|path| path.is_file());

    RunSample {
        grading: grading_path
            .as_ref()
            .and_then(|path| read_json_file(path).ok()),
        timing: timing_path
            .as_ref()
            .and_then(|path| read_json_file(path).ok()),
    }
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> std::result::Result<T, EvalError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn normalize_assertion_key(assertion: &str) -> String {
    assertion.trim().to_string()
}

fn pass_rate(passed: u32, failed: u32) -> f64 {
    let total = passed + failed;
    if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    }
}

fn detect_timing_outliers(
    by_scenario: HashMap<ScenarioKind, Vec<ScenarioMetricSample>>,
) -> Vec<TimingOutlierRecord> {
    detect_outliers(by_scenario)
        .into_iter()
        .map(|(sample, scenario, mean, sigma)| TimingOutlierRecord {
            eval_id: sample.eval_id,
            scenario: scenario.as_str().to_string(),
            attempt: sample.attempt,
            duration_ms: sample.value,
            mean_ms: mean,
            stddev_ms: sigma,
        })
        .collect()
}

fn detect_token_outliers(
    by_scenario: HashMap<ScenarioKind, Vec<ScenarioMetricSample>>,
) -> Vec<TokenOutlierRecord> {
    detect_outliers(by_scenario)
        .into_iter()
        .map(|(sample, scenario, mean, sigma)| TokenOutlierRecord {
            eval_id: sample.eval_id,
            scenario: scenario.as_str().to_string(),
            attempt: sample.attempt,
            total_tokens: sample.value,
            mean_ms: mean,
            stddev_ms: sigma,
        })
        .collect()
}

fn detect_outliers(
    by_scenario: HashMap<ScenarioKind, Vec<ScenarioMetricSample>>,
) -> Vec<(ScenarioMetricSample, ScenarioKind, f64, f64)> {
    let mut outliers = Vec::new();

    for (scenario, samples) in by_scenario {
        if samples.len() < 4 {
            continue;
        }

        let values: Vec<u64> = samples.iter().map(|sample| sample.value).collect();
        let mean = values.iter().map(|value| *value as f64).sum::<f64>() / values.len() as f64;
        let sigma = stddev(&values, mean);
        if sigma == 0.0 {
            continue;
        }

        let threshold = mean + 2.0 * sigma;
        for sample in samples {
            if sample.value as f64 > threshold {
                outliers.push((sample, scenario, mean, sigma));
            }
        }
    }

    outliers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_report(
        report_dir: &Path,
        runs: serde_json::Value,
        iteration: u32,
        report_id: &str,
    ) {
        fs::create_dir_all(report_dir).unwrap();
        let report = serde_json::json!({
            "schema_version": "trg.skills-eval.report.v1",
            "report": {
                "id": report_id,
                "generated_at": "2026-05-26T00:00:00Z",
                "iteration": iteration,
                "producer": { "name": "trg", "version": "0.3.0" }
            },
            "suite": {
                "skill_name": "demo",
                "skill_path": "demo",
                "skill_hash": "sha256:abc",
                "evals_path": "demo/evals/evals.json",
                "evals_hash": "sha256:def"
            },
            "dimensions": {
                "eval_cases": [],
                "assertions": [],
                "skill_revisions": [],
                "model_configs": [],
                "scenarios": [],
                "graders": []
            },
            "runs": runs,
            "assertion_results": [],
            "summaries": { "by_scenario": [] },
            "comparisons": []
        });
        fs::write(
            report_dir.join("report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    fn write_run_artifacts(
        report_dir: &Path,
        run_id: &str,
        grading: Option<&str>,
        timing: Option<&str>,
    ) {
        let run_dir = report_dir.join(format!("runs/{run_id}"));
        let workspace = run_dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        if let Some(grading) = grading {
            fs::write(run_dir.join("grading.json"), grading).unwrap();
        }
        if let Some(timing) = timing {
            fs::write(run_dir.join("timing.json"), timing).unwrap();
        }
    }

    fn sample_run(
        id: &str,
        eval_case_id: &str,
        scenario: &str,
        attempt: u32,
        status: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "eval_case_id": eval_case_id,
            "scenario_id": scenario,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": attempt,
            "status": status,
            "paths": { "workspace": format!("runs/{id}/workspace") },
            "artifacts": [],
            "metrics": {}
        })
    }

    #[test]
    fn detects_always_pass_assertions() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.always_pass.len(), 1);
        assert_eq!(summary.always_pass[0].eval_id, "case-a");
        assert_eq!(summary.always_pass[0].assertion_text, "stable pass");
        assert_eq!(summary.always_pass[0].attempts_observed, 2);
    }

    #[test]
    fn detects_always_fail_assertions() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 2, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.always_fail.len(), 1);
        assert_eq!(summary.always_fail[0].attempts_observed, 2);
    }

    #[test]
    fn detects_helped_by_skill_assertions() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.helped_by_skill.len(), 1);
        assert_eq!(summary.helped_by_skill[0].with_skill_pass_rate, 1.0);
        assert_eq!(summary.helped_by_skill[0].without_skill_pass_rate, 0.0);
        assert!((summary.helped_by_skill[0].delta - 1.0).abs() < 0.0001);
    }

    #[test]
    fn detects_flaky_assertions_across_attempts() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.flaky_assertions.len(), 1);
        assert_eq!(summary.flaky_assertions[0].attempts, 2);
        assert_eq!(summary.flaky_assertions[0].pass_count, 1);
        assert!((summary.flaky_assertions[0].flakiness_ratio - 0.5).abs() < 0.0001);
    }

    #[test]
    fn outlier_detection_requires_at_least_four_samples() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
                sample_run("run-003", "case-a", "with_skill", 3, "completed"),
            ]),
            1,
            "report-a",
        );
        for (run_id, duration_ms, tokens) in [
            ("run-001", 1000, 100),
            ("run-002", 1100, 110),
            ("run-003", 9000, 900),
        ] {
            write_run_artifacts(
                temp.path(),
                run_id,
                None,
                Some(&format!(
                    r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
                )),
            );
        }

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();
        assert!(summary.timing_outliers.is_empty());
        assert!(summary.token_outliers.is_empty());

        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
                sample_run("run-003", "case-a", "with_skill", 3, "completed"),
                sample_run("run-004", "case-a", "with_skill", 4, "completed"),
                sample_run("run-005", "case-a", "with_skill", 5, "completed"),
                sample_run("run-006", "case-a", "with_skill", 6, "completed"),
                sample_run("run-007", "case-a", "with_skill", 7, "completed"),
                sample_run("run-008", "case-a", "with_skill", 8, "completed"),
            ]),
            1,
            "report-a",
        );
        for (run_id, duration_ms, tokens) in [
            ("run-001", 1000, 1000),
            ("run-002", 1000, 1000),
            ("run-003", 1000, 1000),
            ("run-004", 1000, 1000),
            ("run-005", 1000, 1000),
            ("run-006", 1000, 1000),
            ("run-007", 1000, 1000),
            ("run-008", 50000, 50000),
        ] {
            write_run_artifacts(
                temp.path(),
                run_id,
                None,
                Some(&format!(
                    r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
                )),
            );
        }

        let summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();
        assert_eq!(summary.timing_outliers.len(), 1);
        assert_eq!(summary.timing_outliers[0].attempt, 8);
        assert_eq!(summary.timing_outliers[0].duration_ms, 50000);
        assert_eq!(summary.token_outliers.len(), 1);
        assert_eq!(summary.token_outliers[0].total_tokens, 50000);
    }

    #[test]
    fn cross_iteration_deltas_surface_newly_stable_assertions() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");

        write_report(
            &previous,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            1,
            "report-iter-1",
        );
        write_run_artifacts(
            &previous,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":false}]}"#),
            None,
        );

        write_report(
            &current,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            2,
            "report-iter-2",
        );
        for run_id in ["run-001", "run-002"] {
            write_run_artifacts(
                &current,
                run_id,
                Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
                None,
            );
        }

        let summary = build_iteration_summary_document(
            &current,
            IterationSummaryOptions {
                previous_report_dir: Some(previous),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap();

        let cross = summary.cross_iteration.as_ref().unwrap();
        assert_eq!(cross.newly_always_pass.len(), 1);
        assert_eq!(summary.always_pass[0].cross_iteration_delta, Some(CrossIterationDelta::New));
    }

    #[test]
    fn iteration_summary_json_matches_schema() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
            Some(r#"{ "duration_ms": 1000, "total_tokens": 100 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"a","passed":false}]}"#),
            Some(r#"{ "duration_ms": 2000, "total_tokens": 200 }"#),
        );

        let mut summary = build_iteration_summary_document(
            temp.path(),
            IterationSummaryOptions::default(),
        )
        .unwrap();
        summary.generated_at = "2026-05-26T12:00:00Z".to_string();

        let json = serde_json::to_value(&summary).unwrap();
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../schemas/iteration-summary.json.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }
}
