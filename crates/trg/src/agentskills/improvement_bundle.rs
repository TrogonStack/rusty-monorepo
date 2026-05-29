//! Improvement bundles aggregate failed assertions, human feedback, and transcript
//! excerpts from a completed eval iteration so reviewers can revise the skill.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::eval_suite_drift::{detect_eval_suite_drift_vs_skill, maybe_emit_eval_suite_drift_warning};
use super::evals::{EvalError, Result};
use super::feedback::{load_run_feedback_entries, FeedbackNote};
use super::grading::GradingFile;
use super::report::{ReportDocument, RunRecord, ScenarioKind};

pub const SCHEMA_VERSION: &str = "trg.skills-eval.improvement-bundle.v1";
pub const BUNDLE_MD_NAME: &str = "improvement-bundle.md";
pub const BUNDLE_JSON_NAME: &str = "improvement-bundle.json";
pub const NEXT_ITERATION_DIR: &str = "next-iteration";

pub const DEFAULT_EXCERPT_LINES: usize = 200;

#[derive(Debug, Clone)]
pub struct NextIterationOptions {
    pub allow_eval_suite_drift: bool,
    pub skill_dir: Option<PathBuf>,
    pub excerpt_lines: usize,
}

impl Default for NextIterationOptions {
    fn default() -> Self {
        Self {
            allow_eval_suite_drift: false,
            skill_dir: None,
            excerpt_lines: DEFAULT_EXCERPT_LINES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct EvalSuiteDrift {
    pub detected: bool,
    pub previous_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_evals_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_eval_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_eval_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct BundleSummary {
    pub iteration: u32,
    pub report_id: String,
    pub skill_name: String,
    pub skill_hash: String,
    pub evals_hash: String,
    pub total_runs: usize,
    pub passed_assertions: usize,
    pub failed_assertions: usize,
    pub completed_runs: usize,
    pub failed_runs: usize,
    pub skipped_runs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct FailedAssertionEntry {
    pub run_id: String,
    pub scenario_id: ScenarioKind,
    pub attempt: u32,
    pub assertion: String,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_feedback: Option<HumanFeedbackAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct HumanFeedbackAttachment {
    pub source_path: String,
    pub reviewer: String,
    pub reviewed_at: String,
    pub notes: Vec<FeedbackNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct FailedAssertionGroup {
    pub eval_case_id: String,
    pub eval_slug: String,
    pub failures: Vec<FailedAssertionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct HumanFeedbackGroup {
    pub eval_case_id: String,
    pub eval_slug: String,
    pub scenario_id: ScenarioKind,
    pub run_id: String,
    pub feedback: HumanFeedbackAttachment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, JsonSchema)]
pub struct HumanFeedbackSectionSummary {
    pub reviewed_no_issues_runs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct TranscriptExcerpt {
    pub run_id: String,
    pub eval_case_id: String,
    pub eval_slug: String,
    pub scenario_id: ScenarioKind,
    pub path: String,
    pub total_lines: usize,
    pub truncated: bool,
    pub head: Vec<String>,
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct ImprovementBundleDocument {
    pub schema_version: String,
    pub generated_at: String,
    pub source_report_dir: String,
    pub output_dir: String,
    pub summary: BundleSummary,
    pub eval_suite_drift: EvalSuiteDrift,
    pub failed_assertions: Vec<FailedAssertionGroup>,
    pub human_feedback: Vec<HumanFeedbackGroup>,
    pub human_feedback_summary: HumanFeedbackSectionSummary,
    pub transcript_excerpts: Vec<TranscriptExcerpt>,
    pub suggested_focus: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImprovementBundleOutput {
    pub output_dir: PathBuf,
    pub markdown_path: PathBuf,
    pub json_path: PathBuf,
    pub document: ImprovementBundleDocument,
}

pub fn next_iteration_output_dir(from_report_dir: &Path) -> PathBuf {
    from_report_dir
        .parent()
        .map(|parent| parent.join(NEXT_ITERATION_DIR))
        .unwrap_or_else(|| PathBuf::from(NEXT_ITERATION_DIR))
}

pub fn build_improvement_bundle(
    from_report_dir: &Path,
    options: NextIterationOptions,
) -> Result<ImprovementBundleDocument> {
    let report_path = from_report_dir.join("report.json");
    if !report_path.is_file() {
        return Err(EvalError::Validation(
            super::validation::ValidationError::for_field(
                "report_dir",
                format!("report.json not found in {}", from_report_dir.display()),
            )
            .into(),
        ));
    }

    let content = std::fs::read_to_string(&report_path)?;
    let report: ReportDocument = serde_json::from_str(&content)?;

    let output_dir = next_iteration_output_dir(from_report_dir);
    let skill_path = options
        .skill_dir
        .as_deref()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&report.suite.skill_path));
    let drift_report = detect_eval_suite_drift_vs_skill(&report, &skill_path)?;
    maybe_emit_eval_suite_drift_warning(drift_report.as_ref(), options.allow_eval_suite_drift);
    let eval_suite_drift = eval_suite_drift_from_report(&report, &skill_path, drift_report)?;

    let failed_assertions = collect_failed_assertion_groups(from_report_dir, &report)?;
    let (feedback_by_run, human_feedback_summary) = index_run_feedback(from_report_dir)?;
    let failed_assertions = attach_feedback_to_failures(failed_assertions, &feedback_by_run);
    let human_feedback = collect_human_feedback_groups(&report, &feedback_by_run);
    let failed_run_ids = failed_run_ids(from_report_dir, &report, &failed_assertions);
    let excerpt_lines = options.excerpt_lines.max(1);
    let transcript_excerpts = collect_transcript_excerpts(from_report_dir, &report, &failed_run_ids, excerpt_lines);
    let suggested_focus = derive_suggested_focus(from_report_dir, &report, &failed_assertions);
    let summary = build_summary(&report, from_report_dir)?;

    Ok(ImprovementBundleDocument {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        source_report_dir: from_report_dir.display().to_string(),
        output_dir: output_dir.display().to_string(),
        summary,
        eval_suite_drift,
        failed_assertions,
        human_feedback,
        human_feedback_summary,
        transcript_excerpts,
        suggested_focus,
    })
}

pub fn write_improvement_bundle(
    from_report_dir: &Path,
    options: NextIterationOptions,
) -> Result<ImprovementBundleOutput> {
    let document = build_improvement_bundle(from_report_dir, options)?;
    let output_dir = PathBuf::from(&document.output_dir);
    std::fs::create_dir_all(&output_dir)?;

    let markdown_path = output_dir.join(BUNDLE_MD_NAME);
    let json_path = output_dir.join(BUNDLE_JSON_NAME);

    std::fs::write(&markdown_path, render_improvement_bundle_markdown(&document))?;
    std::fs::write(&json_path, serde_json::to_string_pretty(&document)?)?;

    Ok(ImprovementBundleOutput {
        output_dir,
        markdown_path,
        json_path,
        document,
    })
}

fn eval_suite_drift_from_report(
    report: &ReportDocument,
    skill_path: &Path,
    drift_report: Option<super::eval_suite_drift::EvalSuiteDriftReport>,
) -> Result<EvalSuiteDrift> {
    let evals_path = skill_path.join("evals").join("evals.json");
    let previous_hash = report.suite.evals_hash.clone();
    let detected = drift_report.is_some();
    let (current_hash, added_eval_ids, removed_eval_ids) = if let Some(drift) = drift_report {
        (Some(drift.current_hash), drift.added_eval_ids, drift.removed_eval_ids)
    } else {
        (None, Vec::new(), Vec::new())
    };

    let warning = if detected {
        let current = current_hash.as_deref().unwrap_or("unknown");
        Some(format!(
            "WARN: eval suite changed between iterations (previous {previous_hash}, current {current})"
        ))
    } else {
        None
    };

    Ok(EvalSuiteDrift {
        detected,
        previous_hash,
        current_hash,
        current_evals_path: Some(evals_path.display().to_string()),
        added_eval_ids,
        removed_eval_ids,
        warning,
    })
}

fn build_summary(report: &ReportDocument, report_dir: &Path) -> Result<BundleSummary> {
    let mut passed_assertions = 0usize;
    let mut failed_assertions = 0usize;
    let mut completed_runs = 0usize;
    let mut failed_runs = 0usize;
    let mut skipped_runs = 0usize;

    for run in &report.runs {
        match run.status.as_str() {
            "failed" => failed_runs += 1,
            "skipped" => skipped_runs += 1,
            "completed" => completed_runs += 1,
            _ => {}
        }

        let grading_path = grading_path_for_run(report_dir, run);
        if !grading_path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&grading_path) else {
            continue;
        };
        let Ok(grading) = serde_json::from_str::<GradingFile>(&content) else {
            continue;
        };
        for result in &grading.assertion_results {
            if result.passed {
                passed_assertions += 1;
            } else {
                failed_assertions += 1;
            }
        }
    }

    Ok(BundleSummary {
        iteration: report.report.iteration,
        report_id: report.report.id.clone(),
        skill_name: report.suite.skill_name.clone(),
        skill_hash: report.suite.skill_hash.clone(),
        evals_hash: report.suite.evals_hash.clone(),
        total_runs: report.runs.len(),
        passed_assertions,
        failed_assertions,
        completed_runs,
        failed_runs,
        skipped_runs,
    })
}

fn collect_failed_assertion_groups(report_dir: &Path, report: &ReportDocument) -> Result<Vec<FailedAssertionGroup>> {
    let slug_by_case: HashMap<&str, &str> = report
        .dimensions
        .eval_cases
        .iter()
        .map(|eval_case| (eval_case.id.as_str(), eval_case.slug.as_str()))
        .collect();

    let mut groups: BTreeMap<String, FailedAssertionGroup> = BTreeMap::new();

    for run in &report.runs {
        let grading_path = grading_path_for_run(report_dir, run);
        if !grading_path.is_file() {
            continue;
        }

        let grading: GradingFile = serde_json::from_str(&std::fs::read_to_string(&grading_path)?)?;
        for result in grading.assertion_results.iter().filter(|result| !result.passed) {
            let group = groups
                .entry(run.eval_case_id.clone())
                .or_insert_with(|| FailedAssertionGroup {
                    eval_case_id: run.eval_case_id.clone(),
                    eval_slug: slug_by_case
                        .get(run.eval_case_id.as_str())
                        .map(|slug| (*slug).to_string())
                        .unwrap_or_else(|| run.eval_slug.clone()),
                    failures: Vec::new(),
                });
            group.failures.push(FailedAssertionEntry {
                run_id: run.id.clone(),
                scenario_id: run.scenario_id,
                attempt: run.attempt,
                assertion: result.assertion.clone(),
                evidence: result.evidence.clone(),
                human_feedback: None,
            });
        }
    }

    Ok(groups.into_values().collect())
}

fn index_run_feedback(
    report_dir: &Path,
) -> Result<(HashMap<String, HumanFeedbackAttachment>, HumanFeedbackSectionSummary)> {
    let entries = load_run_feedback_entries(report_dir)?;
    let mut feedback_by_run = HashMap::new();
    let mut reviewed_no_issues_runs = 0usize;

    for entry in entries {
        if entry.feedback.notes.is_empty() {
            reviewed_no_issues_runs += 1;
            continue;
        }

        feedback_by_run.insert(
            entry.run_id.clone(),
            HumanFeedbackAttachment {
                source_path: entry.source_path,
                reviewer: entry.feedback.reviewer,
                reviewed_at: entry.feedback.reviewed_at,
                notes: entry.feedback.notes,
            },
        );
    }

    Ok((
        feedback_by_run,
        HumanFeedbackSectionSummary {
            reviewed_no_issues_runs,
        },
    ))
}

fn attach_feedback_to_failures(
    mut groups: Vec<FailedAssertionGroup>,
    feedback_by_run: &HashMap<String, HumanFeedbackAttachment>,
) -> Vec<FailedAssertionGroup> {
    for group in &mut groups {
        for failure in &mut group.failures {
            failure.human_feedback = feedback_by_run.get(&failure.run_id).cloned();
        }
    }
    groups
}

fn collect_human_feedback_groups(
    report: &ReportDocument,
    feedback_by_run: &HashMap<String, HumanFeedbackAttachment>,
) -> Vec<HumanFeedbackGroup> {
    let slug_by_case: HashMap<&str, &str> = report
        .dimensions
        .eval_cases
        .iter()
        .map(|eval_case| (eval_case.id.as_str(), eval_case.slug.as_str()))
        .collect();

    let mut groups: BTreeMap<(String, ScenarioKind), HumanFeedbackGroup> = BTreeMap::new();

    for run in &report.runs {
        let Some(feedback) = feedback_by_run.get(&run.id) else {
            continue;
        };

        groups.insert(
            (run.eval_case_id.clone(), run.scenario_id),
            HumanFeedbackGroup {
                eval_case_id: run.eval_case_id.clone(),
                eval_slug: slug_by_case
                    .get(run.eval_case_id.as_str())
                    .map(|slug| (*slug).to_string())
                    .unwrap_or_else(|| run.eval_slug.clone()),
                scenario_id: run.scenario_id,
                run_id: run.id.clone(),
                feedback: feedback.clone(),
            },
        );
    }

    groups.into_values().collect()
}

fn failed_run_ids(report_dir: &Path, report: &ReportDocument, groups: &[FailedAssertionGroup]) -> HashSet<String> {
    let mut ids: HashSet<String> = groups
        .iter()
        .flat_map(|group| group.failures.iter().map(|failure| failure.run_id.clone()))
        .collect();

    for run in &report.runs {
        if run.status == "failed" {
            ids.insert(run.id.clone());
        }
        if run_has_failed_grading(report_dir, run) {
            ids.insert(run.id.clone());
        }
    }

    ids
}

fn run_has_failed_grading(report_dir: &Path, run: &RunRecord) -> bool {
    let grading_path = grading_path_for_run(report_dir, run);
    if !grading_path.is_file() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&grading_path) else {
        return false;
    };
    let Ok(grading) = serde_json::from_str::<GradingFile>(&content) else {
        return false;
    };
    grading.assertion_results.iter().any(|result| !result.passed)
}

fn collect_transcript_excerpts(
    report_dir: &Path,
    report: &ReportDocument,
    failed_run_ids: &HashSet<String>,
    excerpt_lines: usize,
) -> Vec<TranscriptExcerpt> {
    let mut excerpts = Vec::new();

    for run in &report.runs {
        if !failed_run_ids.contains(&run.id) {
            continue;
        }

        let transcript_path = transcript_path_for_run(report_dir, run);
        if !transcript_path.is_file() {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&transcript_path) else {
            continue;
        };
        let lines: Vec<String> = content.lines().map(str::to_string).collect();
        let total_lines = lines.len();
        let (head, tail, truncated) = excerpt_lines_from_lines(&lines, excerpt_lines);

        excerpts.push(TranscriptExcerpt {
            run_id: run.id.clone(),
            eval_case_id: run.eval_case_id.clone(),
            eval_slug: run.eval_slug.clone(),
            scenario_id: run.scenario_id,
            path: transcript_path
                .strip_prefix(report_dir)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| transcript_path.display().to_string()),
            total_lines,
            truncated,
            head,
            tail,
        });
    }

    excerpts
}

fn excerpt_lines_from_lines(lines: &[String], max_lines: usize) -> (Vec<String>, Vec<String>, bool) {
    if lines.len() <= max_lines * 2 {
        return (lines.to_vec(), Vec::new(), false);
    }

    let head = lines[..max_lines].to_vec();
    let tail = lines[lines.len() - max_lines..].to_vec();
    (head, tail, true)
}

fn derive_suggested_focus(report_dir: &Path, report: &ReportDocument, groups: &[FailedAssertionGroup]) -> Vec<String> {
    let mut focus = Vec::new();

    let mut assertion_run_counts: HashMap<String, HashSet<String>> = HashMap::new();
    for group in groups {
        for failure in &group.failures {
            assertion_run_counts
                .entry(failure.assertion.clone())
                .or_default()
                .insert(failure.run_id.clone());
        }
    }

    for (assertion, run_ids) in assertion_run_counts {
        if run_ids.len() > 1 {
            focus.push(format!("Assertion failing across {} runs: {assertion}", run_ids.len()));
        }
    }

    focus.extend(underperforming_with_skill_focus(report_dir, report));
    focus.sort();
    focus.dedup();
    focus
}

fn underperforming_with_skill_focus(report_dir: &Path, report: &ReportDocument) -> Vec<String> {
    let mut by_case_scenario: HashMap<(String, ScenarioKind), (usize, usize)> = HashMap::new();

    for run in &report.runs {
        let grading_path = grading_path_for_run(report_dir, run);
        if !grading_path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&grading_path) else {
            continue;
        };
        let Ok(grading) = serde_json::from_str::<GradingFile>(&content) else {
            continue;
        };

        let entry = by_case_scenario
            .entry((run.eval_case_id.clone(), run.scenario_id))
            .or_insert((0, 0));
        for result in &grading.assertion_results {
            if result.passed {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }

    let eval_cases: HashSet<String> = report.runs.iter().map(|run| run.eval_case_id.clone()).collect();
    let mut focus = Vec::new();

    for eval_case_id in eval_cases {
        let with_rates = pass_rate(by_case_scenario.get(&(eval_case_id.clone(), ScenarioKind::WithSkill)));
        let without_rates = pass_rate(by_case_scenario.get(&(eval_case_id.clone(), ScenarioKind::WithoutSkill)));

        if without_rates > with_rates {
            focus.push(format!(
                "Eval case {eval_case_id}: with_skill pass rate ({with_rates:.0}%) below without_skill ({without_rates:.0}%)"
            ));
        }
    }

    focus
}

fn pass_rate(entry: Option<&(usize, usize)>) -> f64 {
    let Some((passed, failed)) = entry else {
        return 0.0;
    };
    let total = passed + failed;
    if total == 0 {
        0.0
    } else {
        (*passed as f64 / total as f64) * 100.0
    }
}

fn grading_path_for_run(report_dir: &Path, run: &RunRecord) -> PathBuf {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());

    [run_dir.join("grading.json"), workspace_dir.join("grading.json")]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| run_dir.join("grading.json"))
}

fn transcript_path_for_run(report_dir: &Path, run: &RunRecord) -> PathBuf {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());
    run_dir.join("transcript.jsonl")
}

pub fn render_improvement_bundle_markdown(document: &ImprovementBundleDocument) -> String {
    let mut md = String::new();

    md.push_str("# Improvement Bundle\n\n");

    md.push_str("## Summary\n\n");
    md.push_str(&format!("- Iteration: {}\n", document.summary.iteration));
    md.push_str(&format!("- Report ID: {}\n", document.summary.report_id));
    md.push_str(&format!(
        "- Skill: {} ({})\n",
        document.summary.skill_name, document.summary.skill_hash
    ));
    md.push_str(&format!("- Eval suite hash: {}\n", document.summary.evals_hash));
    md.push_str(&format!("- Total runs: {}\n", document.summary.total_runs));
    md.push_str(&format!(
        "- Run outcomes: {} completed, {} failed, {} skipped\n",
        document.summary.completed_runs, document.summary.failed_runs, document.summary.skipped_runs
    ));
    md.push_str(&format!(
        "- Assertions: {} passed, {} failed\n",
        document.summary.passed_assertions, document.summary.failed_assertions
    ));
    md.push_str(&format!("- Source report: {}\n", document.source_report_dir));
    if document.eval_suite_drift.detected {
        if let Some(warning) = &document.eval_suite_drift.warning {
            md.push_str(&format!("- {warning}\n"));
        } else {
            md.push_str("- WARN: eval suite changed between iterations\n");
        }
    }
    md.push('\n');

    md.push_str("## Failed Assertions\n\n");
    if document.failed_assertions.is_empty() {
        md.push_str("_No failed assertions._\n\n");
    } else {
        for group in &document.failed_assertions {
            md.push_str(&format!("### {} (`{}`)\n\n", group.eval_slug, group.eval_case_id));
            for failure in &group.failures {
                md.push_str(&format!(
                    "- **{}** / {} / attempt {} — {}\n  - Evidence: {}\n",
                    failure.run_id,
                    failure.scenario_id.as_str(),
                    failure.attempt,
                    failure.assertion,
                    failure.evidence
                ));
            }
            md.push('\n');
        }
    }

    md.push_str("## Human Feedback\n\n");
    if document.human_feedback_summary.reviewed_no_issues_runs > 0 {
        md.push_str(&format!(
            "_{} run(s) reviewed with no issues (empty feedback notes)._\n\n",
            document.human_feedback_summary.reviewed_no_issues_runs
        ));
    }
    if document.human_feedback.is_empty() {
        if document.human_feedback_summary.reviewed_no_issues_runs == 0 {
            md.push_str("_No human feedback notes._\n\n");
        }
    } else {
        for entry in &document.human_feedback {
            md.push_str(&format!(
                "### {} / {} (`{}`)\n\n",
                entry.eval_slug,
                entry.scenario_id.as_str(),
                entry.run_id
            ));
            md.push_str(&format!("_Source: {}_\n", entry.feedback.source_path));
            md.push_str(&format!(
                "_Reviewer: {} at {}_\n\n",
                entry.feedback.reviewer, entry.feedback.reviewed_at
            ));
            for note in &entry.feedback.notes {
                md.push_str(&format!("- [{:?}/{:?}] {}\n", note.severity, note.category, note.text));
            }
            md.push('\n');
        }
    }

    md.push_str("## Transcript Excerpts\n\n");
    if document.transcript_excerpts.is_empty() {
        md.push_str("_No transcript excerpts for failed runs._\n\n");
    } else {
        for excerpt in &document.transcript_excerpts {
            md.push_str(&format!(
                "### {} / {} / {} (`{}`)\n\n",
                excerpt.eval_slug,
                excerpt.scenario_id.as_str(),
                excerpt.run_id,
                excerpt.path
            ));
            md.push_str(&format!(
                "_Total lines: {} ({})_\n\n",
                excerpt.total_lines,
                if excerpt.truncated {
                    "truncated to head/tail excerpts"
                } else {
                    "full transcript"
                }
            ));
            if !excerpt.head.is_empty() {
                md.push_str("```\n");
                for line in &excerpt.head {
                    md.push_str(line);
                    md.push('\n');
                }
                if excerpt.truncated {
                    md.push_str("...\n");
                    for line in &excerpt.tail {
                        md.push_str(line);
                        md.push('\n');
                    }
                }
                md.push_str("```\n\n");
            }
        }
    }

    md.push_str("## Suggested Focus\n\n");
    if document.suggested_focus.is_empty() {
        md.push_str("_No auto-derived focus areas._\n\n");
    } else {
        for item in &document.suggested_focus {
            md.push_str(&format!("- {item}\n"));
        }
        md.push('\n');
    }

    md
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::agentskills::feedback::{
        FeedbackCategory, FeedbackDocument, FeedbackNote, FeedbackSeverity, FEEDBACK_FILE_NAME,
    };
    use crate::agentskills::grading::{
        AssertionGradeResult, GraderInfo, GraderKind, GradingSummary, GRADING_SCHEMA_VERSION,
    };
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;
    use crate::fs::FileSystem;
    use std::path::Path;

    fn write_grading(run_dir: &Path, assertion: &str, passed: bool, evidence: &str) {
        std::fs::create_dir_all(run_dir).unwrap();
        let grading = GradingFile {
            schema_version: GRADING_SCHEMA_VERSION.to_string(),
            assertion_results: vec![AssertionGradeResult {
                assertion: assertion.to_string(),
                passed,
                evidence: evidence.to_string(),
                grader: GraderInfo {
                    kind: GraderKind::Mechanical,
                    model: None,
                    command: None,
                },
                rationale: None,
            }],
            summary: GradingSummary {
                passed: usize::from(passed),
                failed: usize::from(!passed),
                total: 1,
                pass_rate: if passed { 1.0 } else { 0.0 },
            },
        };
        std::fs::write(
            run_dir.join("grading.json"),
            serde_json::to_string_pretty(&grading).unwrap(),
        )
        .unwrap();
    }

    pub fn write_feedback(run_dir: &Path, text: &str) {
        let document = FeedbackDocument {
            reviewer: "reviewer@example.com".to_string(),
            reviewed_at: "2026-05-26T12:00:00Z".to_string(),
            notes: vec![FeedbackNote {
                severity: FeedbackSeverity::Warning,
                category: FeedbackCategory::Correctness,
                text: text.to_string(),
            }],
        };
        std::fs::write(
            run_dir.join(FEEDBACK_FILE_NAME),
            serde_json::to_string_pretty(&document).unwrap(),
        )
        .unwrap();
    }

    pub fn write_empty_feedback(run_dir: &Path) {
        let document = FeedbackDocument {
            reviewer: "reviewer@example.com".to_string(),
            reviewed_at: "2026-05-26T12:00:00Z".to_string(),
            notes: Vec::new(),
        };
        std::fs::write(
            run_dir.join(FEEDBACK_FILE_NAME),
            serde_json::to_string_pretty(&document).unwrap(),
        )
        .unwrap();
    }

    fn write_transcript(run_dir: &Path, lines: usize) {
        let content: String = (0..lines).map(|index| format!("line-{index}\n")).collect();
        std::fs::write(run_dir.join("transcript.jsonl"), content).unwrap();
    }

    pub fn sample_prior_iteration_fixture(temp: &tempfile::TempDir, skill_root: &Path) -> PathBuf {
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

        let bundle = build_report_bundle(
            &fs,
            skill_path,
            skill_path,
            "demo-skill",
            "ci-default",
            &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
            BuildReportOptions {
                report_id: Some("report-iter-1".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                iteration: Some(1),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

        write_grading(
            &report_dir.join("runs/run-001"),
            "assert a",
            false,
            "missing file out.json",
        );
        write_grading(
            &report_dir.join("runs/run-002"),
            "assert a",
            true,
            "file exists at outputs/out.json",
        );
        write_grading(
            &report_dir.join("runs/run-003"),
            "assert b",
            false,
            "expected summary section",
        );
        write_grading(
            &report_dir.join("runs/run-004"),
            "assert b",
            true,
            "found summary section in outputs/report.md",
        );

        write_feedback(
            &report_dir.join("runs/run-001"),
            "Add explicit output path guidance for case-a",
        );
        write_transcript(&report_dir.join("runs/run-001"), 450);
        write_transcript(&report_dir.join("runs/run-003"), 10);

        std::fs::create_dir_all(skill_root.join("evals")).unwrap();
        std::fs::write(
            skill_root.join("evals/evals.json"),
            fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            skill_root.join("SKILL.md"),
            fs.read_to_string(&skill_path.join("SKILL.md")).unwrap(),
        )
        .unwrap();

        report_dir
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{sample_prior_iteration_fixture, write_empty_feedback};
    use super::*;
    use crate::agentskills::report::ScenarioKind;

    #[test]
    fn bundle_includes_sections_counts_and_transcript_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        let output = write_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        assert!(output.markdown_path.is_file());
        assert!(output.json_path.is_file());
        assert_eq!(output.output_dir, report_dir.parent().unwrap().join(NEXT_ITERATION_DIR));

        let markdown = std::fs::read_to_string(output.markdown_path).unwrap();
        for section in [
            "## Summary",
            "## Failed Assertions",
            "## Human Feedback",
            "## Transcript Excerpts",
            "## Suggested Focus",
        ] {
            assert!(markdown.contains(section), "missing {section}");
        }

        assert_eq!(output.document.failed_assertions.len(), 2);
        assert_eq!(output.document.human_feedback.len(), 1);
        assert_eq!(output.document.transcript_excerpts.len(), 2);
        assert!(output.document.transcript_excerpts[0].truncated);
        assert_eq!(output.document.transcript_excerpts[0].head.len(), DEFAULT_EXCERPT_LINES);
        assert_eq!(output.document.summary.total_runs, 4);
        assert_eq!(output.document.summary.failed_assertions, 2);
        assert!(!output.document.eval_suite_drift.detected);
    }

    #[test]
    fn bundle_warns_on_eval_suite_drift() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        std::fs::write(
            skill_root.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "changed prompt",
                        "expected_output": "output a",
                        "assertions": ["assert a", "assert a2"]
                    }
                ]
            }"#,
        )
        .unwrap();

        let output = write_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        assert!(output.document.eval_suite_drift.detected);
        assert!(output
            .document
            .eval_suite_drift
            .warning
            .as_deref()
            .unwrap_or("")
            .contains("eval suite changed between iterations"));

        let markdown = std::fs::read_to_string(output.markdown_path).unwrap();
        assert!(markdown.contains("WARN: eval suite changed between iterations"));
    }

    #[test]
    fn suggested_focus_flags_underperforming_with_skill() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        let output = write_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        assert!(output
            .document
            .suggested_focus
            .iter()
            .any(|item| item.contains("case-a") && item.contains("with_skill")));
    }

    #[test]
    fn bundle_attaches_matching_feedback_to_failed_assertion_entry() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        let document = build_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        let case_a = document
            .failed_assertions
            .iter()
            .find(|group| group.eval_case_id == "case-a")
            .expect("case-a failures");
        let failure = case_a
            .failures
            .iter()
            .find(|failure| failure.run_id == "run-001")
            .expect("run-001 failure");
        let feedback = failure
            .human_feedback
            .as_ref()
            .expect("feedback attached to failed run");

        assert_eq!(feedback.source_path, "runs/run-001/feedback.json");
        assert_eq!(feedback.notes.len(), 1);
        assert!(feedback.notes[0].text.contains("output path guidance"));

        let grouped = document
            .human_feedback
            .iter()
            .find(|entry| entry.run_id == "run-001")
            .expect("grouped feedback for run-001");
        assert_eq!(grouped.eval_case_id, "case-a");
        assert_eq!(grouped.scenario_id, ScenarioKind::WithSkill);
        assert_eq!(grouped.feedback.source_path, feedback.source_path);
    }

    #[test]
    fn bundle_includes_feedback_without_failed_assertions() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        testutil::write_feedback(
            &report_dir.join("runs/run-002"),
            "Skill prompt should mention CSV headers explicitly",
        );

        let document = build_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        assert!(
            document
                .failed_assertions
                .iter()
                .flat_map(|group| &group.failures)
                .all(|failure| failure.run_id != "run-002"),
            "run-002 has no failed assertions"
        );

        let entry = document
            .human_feedback
            .iter()
            .find(|entry| entry.run_id == "run-002")
            .expect("feedback-only run appears in bundle");
        assert_eq!(entry.eval_case_id, "case-a");
        assert_eq!(entry.scenario_id, ScenarioKind::WithoutSkill);
        assert_eq!(entry.feedback.source_path, "runs/run-002/feedback.json");
    }

    #[test]
    fn bundle_omits_empty_feedback_but_notes_clean_reviews() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        write_empty_feedback(&report_dir.join("runs/run-002"));

        let document = build_improvement_bundle(
            &report_dir,
            NextIterationOptions {
                skill_dir: Some(skill_root),
                ..NextIterationOptions::default()
            },
        )
        .unwrap();

        assert_eq!(document.human_feedback.len(), 1);
        assert!(
            document.human_feedback.iter().all(|entry| entry.run_id != "run-002"),
            "empty feedback must not create a bundle entry"
        );
        assert_eq!(document.human_feedback_summary.reviewed_no_issues_runs, 1);

        let markdown = render_improvement_bundle_markdown(&document);
        assert!(markdown.contains("reviewed with no issues"));
        assert!(!markdown.contains("run-002"));
    }
}
