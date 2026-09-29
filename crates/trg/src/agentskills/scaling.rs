//! Whether a stronger model or configuration actually scores higher on the same suite.
//!
//! The operator lists report bundles weakest to strongest; this never ranks models
//! itself. Each adjacent pair's `with_skill` assertion pass rate is compared with a
//! Newcombe interval the same way a keep-or-revert verdict compares iterations, so a
//! configuration that reads as stronger only because of noise is not read as a real gain,
//! and a case where the stronger configuration scored lower is surfaced as an outlier: the
//! article treats that as a sign of an ambiguous case or a miscalibrated grader.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use thiserror::Error;

use super::benchmark::{aggregate_scenarios_for_records, FailedRunsMode, PassFailSummary};
use super::evals::EvalSplit;
use super::proportion::{self, Interval, Proportion};
use super::report::{ModelConfigDimension, ReportDocument, ScenarioKind};

pub const SCALING_FILE_NAME: &str = "scaling.json";

#[derive(Error, Debug)]
pub enum ScalingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("model scaling compares at least two report bundles weakest to strongest, got {0}")]
    NotEnoughBundles(usize),

    #[error(
        "'{bundle}' was built from suite {actual}, but '{first}' was built from suite {expected}: model scaling only compares configurations run against the same suite"
    )]
    SuiteHashMismatch {
        first: String,
        bundle: String,
        expected: String,
        actual: String,
    },
}

pub type Result<T> = std::result::Result<T, ScalingError>;

/// A report bundle read from disk, kept alongside its directory since aggregating its
/// runs requires reading `grading.json`/`timing.json` back off that same directory.
struct LoadedBundle {
    report_dir: PathBuf,
    document: ReportDocument,
}

fn load_bundle(report_dir: &Path) -> Result<LoadedBundle> {
    let content = std::fs::read_to_string(report_dir.join("report.json"))?;
    let document: ReportDocument = serde_json::from_str(&content)?;
    Ok(LoadedBundle {
        report_dir: report_dir.to_path_buf(),
        document,
    })
}

/// The verdict for one comparison's `with_skill` assertion pass-rate delta, derived from
/// whether the Newcombe interval around `stronger - weaker` excludes zero.
#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ScalingVerdict {
    /// The interval sits entirely above zero: the stronger configuration did better.
    Improved(ScalingDelta),
    /// The interval sits entirely below zero: the stronger configuration did worse.
    Regressed(ScalingDelta),
    /// The interval straddles zero: the configurations have not separated on this suite.
    Flat(ScalingDelta),
    /// Neither bundle scored a `with_skill` run here, so there is nothing to compare.
    /// Never read as `Flat`: silence about a split is not a finding that it held steady.
    NoRuns,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, JsonSchema)]
pub struct ScalingDelta {
    pub weaker_pass_rate: f64,
    pub stronger_pass_rate: f64,
    pub pass_rate_delta: f64,
    pub pass_rate_delta_interval: Interval,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScalingComparison {
    pub overall: ScalingVerdict,
    pub by_split: BTreeMap<EvalSplit, ScalingVerdict>,
}

/// A case the stronger configuration scored lower on than the weaker one, despite the
/// operator naming it as the stronger side. Not itself a verdict on the pair: with few
/// draws a single case can move on noise the same way an overall rate can, but the article
/// treats a case that moves backward as worth an author's look regardless.
#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema)]
pub struct ScalingOutlier {
    pub eval_case_id: String,
    pub weaker_pass_rate: f64,
    pub stronger_pass_rate: f64,
}

/// The model config captured for one bundle in the comparison, so a reader of a step can
/// tell which configuration is named `weaker` and `stronger` without opening its report.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScalingBundleSummary {
    pub report_dir: String,
    pub report_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_config: Option<ModelConfigDimension>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScalingStep {
    pub weaker: ScalingBundleSummary,
    pub stronger: ScalingBundleSummary,
    pub comparison: ScalingComparison,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outliers: Vec<ScalingOutlier>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(title = "trg skills eval scaling")]
pub struct ScalingDocument {
    pub generated_at: String,
    pub suite_evals_hash: String,
    pub bundles: Vec<ScalingBundleSummary>,
    /// Adjacent-pair comparisons, in the weakest-to-strongest order the operator listed
    /// the bundles in.
    pub steps: Vec<ScalingStep>,
    /// The weakest bundle against the strongest, regardless of how many steps sit between
    /// them, so a curve that dips in the middle and recovers by the end is not read as an
    /// end-to-end improvement no single step earned.
    pub end_to_end: ScalingStep,
}

pub fn build_scaling_document(report_dirs: &[PathBuf], failed_runs: FailedRunsMode) -> Result<ScalingDocument> {
    if report_dirs.len() < 2 {
        return Err(ScalingError::NotEnoughBundles(report_dirs.len()));
    }

    let loaded: Vec<LoadedBundle> = report_dirs
        .iter()
        .map(|report_dir| load_bundle(report_dir))
        .collect::<Result<_>>()?;

    let first_dir = loaded[0].report_dir.display().to_string();
    let expected_hash = loaded[0].document.suite.evals_hash.clone();
    for bundle in &loaded[1..] {
        let actual_hash = &bundle.document.suite.evals_hash;
        if *actual_hash != expected_hash {
            return Err(ScalingError::SuiteHashMismatch {
                first: first_dir,
                bundle: bundle.report_dir.display().to_string(),
                expected: expected_hash,
                actual: actual_hash.clone(),
            });
        }
    }

    let bundles: Vec<ScalingBundleSummary> = loaded.iter().map(summarize_bundle).collect();
    let steps: Vec<ScalingStep> = loaded
        .windows(2)
        .map(|pair| build_step(&pair[0], &pair[1], failed_runs))
        .collect();
    let end_to_end = build_step(&loaded[0], &loaded[loaded.len() - 1], failed_runs);

    Ok(ScalingDocument {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        suite_evals_hash: expected_hash,
        bundles,
        steps,
        end_to_end,
    })
}

pub fn write_scaling(report_dir: &Path, document: &ScalingDocument) -> Result<PathBuf> {
    let output_path = report_dir.join(SCALING_FILE_NAME);
    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(&output_path, &json)?;
    Ok(output_path)
}

/// Whether any step, overall or on any split, or the end-to-end comparison, regressed.
pub fn any_regression(document: &ScalingDocument) -> bool {
    document.steps.iter().any(step_regressed) || step_regressed(&document.end_to_end)
}

fn step_regressed(step: &ScalingStep) -> bool {
    matches!(step.comparison.overall, ScalingVerdict::Regressed(_))
        || step
            .comparison
            .by_split
            .values()
            .any(|verdict| matches!(verdict, ScalingVerdict::Regressed(_)))
}

fn summarize_bundle(bundle: &LoadedBundle) -> ScalingBundleSummary {
    ScalingBundleSummary {
        report_dir: bundle.report_dir.display().to_string(),
        report_id: bundle.document.report.id.clone(),
        model_config: bundle.document.dimensions.model_configs.first().cloned(),
    }
}

fn build_step(weaker: &LoadedBundle, stronger: &LoadedBundle, failed_runs: FailedRunsMode) -> ScalingStep {
    let overall = compare_with_skill(weaker, stronger, None, failed_runs);
    let by_split: BTreeMap<EvalSplit, ScalingVerdict> = EvalSplit::ALL
        .into_iter()
        .map(|split| (split, compare_with_skill(weaker, stronger, Some(split), failed_runs)))
        .collect();

    ScalingStep {
        weaker: summarize_bundle(weaker),
        stronger: summarize_bundle(stronger),
        comparison: ScalingComparison { overall, by_split },
        outliers: find_outliers(weaker, stronger, failed_runs),
    }
}

fn compare_with_skill(
    weaker: &LoadedBundle,
    stronger: &LoadedBundle,
    split: Option<EvalSplit>,
    failed_runs: FailedRunsMode,
) -> ScalingVerdict {
    let weaker_summary = with_skill_assertions(weaker, split, failed_runs);
    let stronger_summary = with_skill_assertions(stronger, split, failed_runs);

    let (Some(weaker_proportion), Some(stronger_proportion)) = (
        Proportion::observed(weaker_summary.passed, weaker_summary.failed),
        Proportion::observed(stronger_summary.passed, stronger_summary.failed),
    ) else {
        return ScalingVerdict::NoRuns;
    };

    let interval = proportion::difference(stronger_proportion, weaker_proportion);
    let delta = ScalingDelta {
        weaker_pass_rate: weaker_proportion.rate(),
        stronger_pass_rate: stronger_proportion.rate(),
        pass_rate_delta: stronger_proportion.rate() - weaker_proportion.rate(),
        pass_rate_delta_interval: interval,
    };

    if interval.low() > 0.0 {
        ScalingVerdict::Improved(delta)
    } else if interval.high() < 0.0 {
        ScalingVerdict::Regressed(delta)
    } else {
        ScalingVerdict::Flat(delta)
    }
}

fn with_skill_assertions(
    bundle: &LoadedBundle,
    split: Option<EvalSplit>,
    failed_runs: FailedRunsMode,
) -> PassFailSummary {
    let runs = bundle.document.runs.iter().filter(|run| {
        run.scenario_id == ScenarioKind::WithSkill && split.map(|wanted| run.split == wanted).unwrap_or(true)
    });
    let (scenarios, _deltas) = aggregate_scenarios_for_records(&bundle.report_dir, runs, failed_runs);
    scenarios
        .get(ScenarioKind::WithSkill.as_str())
        .map(|benchmark| benchmark.completed.assertions.clone())
        .unwrap_or_default()
}

fn find_outliers(weaker: &LoadedBundle, stronger: &LoadedBundle, failed_runs: FailedRunsMode) -> Vec<ScalingOutlier> {
    let mut case_ids: BTreeSet<&str> = BTreeSet::new();
    case_ids.extend(weaker.document.runs.iter().map(|run| run.eval_case_id.as_str()));
    case_ids.extend(stronger.document.runs.iter().map(|run| run.eval_case_id.as_str()));

    case_ids
        .into_iter()
        .filter_map(|eval_case_id| {
            let weaker_rate = case_with_skill_pass_rate(weaker, eval_case_id, failed_runs)?;
            let stronger_rate = case_with_skill_pass_rate(stronger, eval_case_id, failed_runs)?;
            (stronger_rate < weaker_rate).then_some(ScalingOutlier {
                eval_case_id: eval_case_id.to_string(),
                weaker_pass_rate: weaker_rate,
                stronger_pass_rate: stronger_rate,
            })
        })
        .collect()
}

fn case_with_skill_pass_rate(bundle: &LoadedBundle, eval_case_id: &str, failed_runs: FailedRunsMode) -> Option<f64> {
    let runs = bundle
        .document
        .runs
        .iter()
        .filter(|run| run.scenario_id == ScenarioKind::WithSkill && run.eval_case_id == eval_case_id);
    let (scenarios, _deltas) = aggregate_scenarios_for_records(&bundle.report_dir, runs, failed_runs);
    let assertions = &scenarios.get(ScenarioKind::WithSkill.as_str())?.completed.assertions;
    (assertions.total > 0).then_some(assertions.pass_rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, WriteReportOptions,
    };
    use crate::fs::testutil::MemFS;

    fn sample_report_dir(temp: &tempfile::TempDir, report_id: &str, model_config: &str) -> PathBuf {
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
                    { "id": "case-a", "prompt": "prompt a long enough", "expected_output": "output a", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "case-b", "prompt": "prompt b long enough", "expected_output": "output b", "graders": [{ "type": "contains", "text": "b" }] }
                ]
            }"#,
        );

        let mut bundle = build_report_bundle(
            &fs,
            skill_path,
            Path::new("demo-skill"),
            "demo-skill",
            model_config,
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some(report_id.to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();

        // A run only lands in the completed bucket `aggregate_scenarios_for_records` reads
        // assertions from once its status says so; the scaffold otherwise leaves every run
        // "skipped" until a harness actually runs it.
        for run in bundle.document.runs.iter_mut() {
            run.status = "completed".to_string();
        }

        write_report_bundle(
            temp.path(),
            &bundle,
            WriteReportOptions {
                force: true,
                ..WriteReportOptions::default()
            },
        )
        .unwrap()
    }

    fn grade_run(report_dir: &Path, run_id: &str, passed: bool) {
        let result = AssertionGradeResult {
            assertion: "assertion text".to_string(),
            passed,
            evidence: "the transcript said so".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            name: None,
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        };
        let grading = build_grading_file(vec![result]).unwrap();
        std::fs::write(
            report_dir.join(format!("runs/{run_id}/grading.json")),
            serde_json::to_string_pretty(&grading).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn fewer_than_two_bundles_is_refused_rather_than_compared_against_itself() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp, "only-one", "solo");

        let result = build_scaling_document(&[report_dir], FailedRunsMode::Bucket);
        assert!(matches!(result, Err(ScalingError::NotEnoughBundles(1))));
    }

    #[test]
    fn bundles_built_from_different_suites_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let weaker_dir = sample_report_dir(&temp, "weaker", "small");

        let fs = MemFS::new();
        let skill_path = Path::new("other-skill");
        fs.insert(
            skill_path.join("SKILL.md"),
            "---\nname: other-skill\ndescription: d\n---\n",
        );
        fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "other-skill",
                "evals": [{ "id": "case-c", "prompt": "a different prompt", "expected_output": "o", "graders": [{ "type": "contains", "text": "c" }] }]
            }"#,
        );
        let other_bundle = build_report_bundle(
            &fs,
            skill_path,
            Path::new("other-skill"),
            "other-skill",
            "large",
            &[ScenarioKind::WithSkill],
            BuildReportOptions {
                report_id: Some("stronger".to_string()),
                generated_at: Some("2026-05-26T12:00:00Z".to_string()),
                ..BuildReportOptions::default()
            },
        )
        .unwrap();
        let stronger_dir = write_report_bundle(
            temp.path(),
            &other_bundle,
            WriteReportOptions {
                force: true,
                ..WriteReportOptions::default()
            },
        )
        .unwrap();

        let result = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket);
        assert!(matches!(result, Err(ScalingError::SuiteHashMismatch { .. })));
    }

    #[test]
    fn a_stronger_config_that_clears_the_noise_reads_as_improved_and_names_its_model_config() {
        let temp = tempfile::tempdir().unwrap();
        let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
        for (run_id, passed) in [("run-001", false), ("run-002", false)] {
            grade_run(&weaker_dir, run_id, passed);
        }
        let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
        for (run_id, passed) in [("run-001", true), ("run-002", true)] {
            grade_run(&stronger_dir, run_id, passed);
        }

        let document =
            build_scaling_document(&[weaker_dir.clone(), stronger_dir.clone()], FailedRunsMode::Bucket).unwrap();

        assert_eq!(document.bundles.len(), 2);
        assert_eq!(
            document.bundles[0]
                .model_config
                .as_ref()
                .map(|config| config.id.as_str()),
            Some("small-model")
        );
        assert_eq!(
            document.bundles[1]
                .model_config
                .as_ref()
                .map(|config| config.id.as_str()),
            Some("large-model")
        );
        assert_eq!(document.steps.len(), 1);
        assert!(matches!(
            document.steps[0].comparison.overall,
            ScalingVerdict::Improved(_)
        ));
        assert!(matches!(
            document.end_to_end.comparison.overall,
            ScalingVerdict::Improved(_)
        ));
        assert!(!any_regression(&document));
    }

    #[test]
    fn a_case_the_stronger_config_scored_lower_on_is_surfaced_as_an_outlier() {
        let temp = tempfile::tempdir().unwrap();
        let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
        // case-a with_skill, then case-b with_skill (build_runs orders eval case then scenario).
        grade_run(&weaker_dir, "run-001", true);
        grade_run(&weaker_dir, "run-002", true);
        let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
        grade_run(&stronger_dir, "run-001", false);
        grade_run(&stronger_dir, "run-002", true);

        let document = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket).unwrap();

        assert_eq!(document.steps[0].outliers.len(), 1);
        assert_eq!(document.steps[0].outliers[0].eval_case_id, "case-a");
    }

    #[test]
    fn a_regression_anywhere_is_reported_by_any_regression() {
        let temp = tempfile::tempdir().unwrap();
        let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
        grade_run(&weaker_dir, "run-001", true);
        grade_run(&weaker_dir, "run-002", true);
        let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
        grade_run(&stronger_dir, "run-001", false);
        grade_run(&stronger_dir, "run-002", false);

        let document = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket).unwrap();

        assert!(any_regression(&document));
    }

    #[test]
    fn writing_puts_scalingjson_in_the_named_report_dir() {
        let temp = tempfile::tempdir().unwrap();
        let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
        grade_run(&weaker_dir, "run-001", true);
        grade_run(&weaker_dir, "run-002", true);
        let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
        grade_run(&stronger_dir, "run-001", true);
        grade_run(&stronger_dir, "run-002", true);

        let document = build_scaling_document(&[weaker_dir, stronger_dir.clone()], FailedRunsMode::Bucket).unwrap();
        let written = write_scaling(&stronger_dir, &document).unwrap();

        assert_eq!(written, stronger_dir.join(SCALING_FILE_NAME));
        assert!(written.is_file());
    }
}
