//! Whether the graders that scored a suite can be trusted, checked against the cases a
//! human actually labelled.
//!
//! A judge vote split (`--grader-votes`) says a judge itself was unsure. It says nothing
//! about the case a judge is unanimous and simply wrong, which only a human label can
//! catch. This joins every scored assertion against the human verdicts recorded in
//! `feedback.json` and reports how often the two agree, per grader and per grader kind,
//! so a grader nobody has checked against a human is visibly unchecked rather than
//! silently assumed correct.

mod constants;
pub use constants::GRADER_AGREEMENT_FILE_NAME;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::feedback::{
    feedback_path_for_run, grading_path_for_run, load_report, AssertionVerdict, HumanVerdict, LoadedReport,
};
use super::grading::telemetry::{phase_span, record_error};
use super::grading::{AssertionGradeResult, GraderKind, GradingFile};
use super::judge_votes::JudgeVoteTally;
use super::proportion::{Interval, Proportion};

#[derive(Error, Debug)]
pub enum GraderAgreementError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, GraderAgreementError>;

impl GraderAgreementError {
    pub(crate) fn error_type(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Json(_) => "json",
            Self::Message(_) => "invalid_report",
        }
    }
}

/// Runs `work` inside `span`, marking it failed when `work` fails.
fn phase<T>(span: tracing::Span, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let result = span.in_scope(work);
    if let Err(error) = &result {
        record_error(&span, error.error_type());
    }
    result
}

impl From<super::feedback::FeedbackError> for GraderAgreementError {
    fn from(value: super::feedback::FeedbackError) -> Self {
        GraderAgreementError::Message(value.to_string())
    }
}

/// The lower bound `--min-agreement` gates on.
///
/// Distinct from `HeadroomThreshold` despite the identical `(0, 1]` shape: a headroom
/// threshold names a ceiling a with_skill arm has run out of room below, and this names a
/// floor an LLM grader's trustworthiness is not allowed to fall under. Folding the two
/// together would make one of the two read the other's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AgreementThreshold(f64);

impl AgreementThreshold {
    pub fn parse(value: f64) -> std::result::Result<Self, String> {
        if !value.is_finite() || value <= 0.0 || value > 1.0 {
            return Err(format!("a minimum agreement of {value} is not a proportion in (0, 1]"));
        }
        Ok(Self(value))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for AgreementThreshold {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = f64::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for AgreementThreshold {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AgreementThreshold".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, for the
    /// same reason `HeadroomThreshold` is: a schema looser than its own parser would call
    /// a document conformant that its own builder would have refused to produce.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "The LLM-grader agreement interval's lower bound must be at least this proportion. Greater than 0 and at most 1.",
            "type": "number",
            "exclusiveMinimum": 0.0,
            "maximum": 1.0
        })
    }
}

impl fmt::Display for AgreementThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AgreementThreshold {
    type Err = String;

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The established command-line parser contract returns a field-specific String validation message instead of a source-bearing error"
        )
    )]
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let parsed: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not a minimum agreement"))?;
        Self::parse(parsed)
    }
}

/// Agreement counted over a set of (run, assertion) pairs a grader scored and a human
/// also labelled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgreementBucket {
    pub agreements: usize,
    pub disagreements: usize,
    /// `agreements / (agreements + disagreements)`. Absent when nothing in this bucket
    /// has both a grader verdict and a human label yet.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub agreement_rate: Option<f64>,
    /// The Wilson 95% interval around `agreement_rate`. Absent for the same reason.
    pub interval: Option<Interval>,
}

impl AgreementBucket {
    fn from_counts(agreements: usize, disagreements: usize) -> Self {
        let proportion = Proportion::observed(agreements, disagreements);
        Self {
            agreements,
            disagreements,
            agreement_rate: proportion.map(Proportion::rate),
            interval: proportion.map(Proportion::interval),
        }
    }

    fn empty() -> Self {
        Self::from_counts(0, 0)
    }
}

/// One grader kind's agreement bucket, for every kind that can appear in a `case.graders`
/// entry. `needs_llm` and `none` are deliberately excluded: the former never survives
/// grading and the latter marks run-integrity results (mock and read-only-fixture
/// violations) that carry no stable assertion id to label in the first place.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgreementByGraderKind {
    pub mechanical: AgreementBucket,
    pub declarative: AgreementBucket,
    pub script: AgreementBucket,
    pub llm: AgreementBucket,
}

impl AgreementByGraderKind {
    fn empty() -> Self {
        Self {
            mechanical: AgreementBucket::empty(),
            declarative: AgreementBucket::empty(),
            script: AgreementBucket::empty(),
            llm: AgreementBucket::empty(),
        }
    }

    fn bucket_mut(&mut self, kind: GraderKind) -> Option<&mut AgreementBucket> {
        match kind {
            GraderKind::Mechanical => Some(&mut self.mechanical),
            GraderKind::Declarative => Some(&mut self.declarative),
            GraderKind::Script => Some(&mut self.script),
            GraderKind::Llm => Some(&mut self.llm),
            GraderKind::NeedsLlm | GraderKind::None => None,
        }
    }
}

/// A grader's call and a human's call on the same assertion, disagreeing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Disagreement {
    pub run_id: String,
    pub assertion_id: String,
    pub grader_kind: GraderKind,
    pub grader_passed: bool,
    pub grader_evidence: String,
    pub human_verdict: HumanVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_rationale: Option<String>,
    /// How a judge panel split, when the grader that produced this call was a panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<JudgeVoteTally>,
}

/// A scored assertion nobody has labelled yet, named so an author knows exactly which
/// `eval feedback label` call would close the gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnlabeledAssertion {
    pub run_id: String,
    pub assertion_id: String,
    pub grader_kind: GraderKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageSummary {
    /// Scored assertions across every run, regardless of whether a human labelled them.
    pub graded_assertions: usize,
    /// The subset of `graded_assertions` a human has recorded a verdict for.
    pub labeled_assertions: usize,
    pub unlabeled: Vec<UnlabeledAssertion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraderAgreementDocument {
    pub overall: AgreementBucket,
    pub by_grader_kind: AgreementByGraderKind,
    pub disagreements: Vec<Disagreement>,
    pub coverage: CoverageSummary,
}

/// Joins every run's `grading.json` against its `feedback.json` human labels and builds
/// the full agreement report.
///
/// Only assertions a grader actually scored are considered: an excluded, unsupported, or
/// ungraded result never rendered a pass/fail call, so there is nothing for a human
/// verdict to agree or disagree with.
pub fn build_grader_agreement_document(report_dir: &Path) -> Result<GraderAgreementDocument> {
    let loaded = phase(phase_span!("load report"), || Ok(load_report(report_dir)?))?;
    phase(phase_span!("collect verdicts"), || {
        collect_agreement(report_dir, &loaded)
    })
}

fn collect_agreement(report_dir: &Path, loaded: &LoadedReport) -> Result<GraderAgreementDocument> {
    let mut overall_agreements = 0usize;
    let mut overall_disagreements = 0usize;
    let mut by_kind = AgreementByGraderKind::empty();
    let mut disagreements = Vec::new();
    let mut graded_assertions = 0usize;
    let mut labeled_assertions = 0usize;
    let mut unlabeled = Vec::new();

    for run in &loaded.runs {
        let grading_path = grading_path_for_run(report_dir, &run.paths.workspace);
        if !grading_path.is_file() {
            continue;
        }
        let grading: GradingFile = serde_json::from_str(&std::fs::read_to_string(&grading_path)?)?;

        let assertion_ids = loaded
            .assertion_ids_by_case
            .get(&run.eval_case_id)
            .cloned()
            .unwrap_or_default();

        let feedback_path = feedback_path_for_run(report_dir, &run.paths.workspace);
        let assertion_verdicts = if feedback_path.is_file() {
            let content = std::fs::read_to_string(&feedback_path)?;
            let document: super::feedback::FeedbackDocument = serde_json::from_str(&content)?;
            document.assertion_verdicts
        } else {
            Default::default()
        };

        for (assertion_id, result) in assertion_ids.iter().zip(grading.assertion_results.iter()) {
            if !result.is_scored() {
                continue;
            }
            graded_assertions += 1;

            let Some(verdict) = assertion_verdicts.get(assertion_id) else {
                unlabeled.push(UnlabeledAssertion {
                    run_id: run.id.clone(),
                    assertion_id: assertion_id.clone(),
                    grader_kind: result.grader.kind,
                });
                continue;
            };
            labeled_assertions += 1;

            record_verdict(
                &run.id,
                assertion_id,
                result,
                verdict,
                &mut overall_agreements,
                &mut overall_disagreements,
                &mut by_kind,
                &mut disagreements,
            );
        }
    }

    disagreements.sort_by(|a, b| {
        (a.run_id.as_str(), a.assertion_id.as_str()).cmp(&(b.run_id.as_str(), b.assertion_id.as_str()))
    });
    unlabeled.sort_by(|a, b| {
        (a.run_id.as_str(), a.assertion_id.as_str()).cmp(&(b.run_id.as_str(), b.assertion_id.as_str()))
    });

    Ok(GraderAgreementDocument {
        overall: AgreementBucket::from_counts(overall_agreements, overall_disagreements),
        by_grader_kind: by_kind.finalized(),
        disagreements,
        coverage: CoverageSummary {
            graded_assertions,
            labeled_assertions,
            unlabeled,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn record_verdict(
    run_id: &str,
    assertion_id: &str,
    result: &AssertionGradeResult,
    verdict: &AssertionVerdict,
    overall_agreements: &mut usize,
    overall_disagreements: &mut usize,
    by_kind: &mut AgreementByGraderKind,
    disagreements: &mut Vec<Disagreement>,
) {
    let agrees = result.passed == verdict.verdict.passed();

    if agrees {
        *overall_agreements += 1;
    } else {
        *overall_disagreements += 1;
    }

    if let Some(bucket) = by_kind.bucket_mut(result.grader.kind) {
        if agrees {
            bucket.agreements += 1;
        } else {
            bucket.disagreements += 1;
        }
    }

    if !agrees {
        disagreements.push(Disagreement {
            run_id: run_id.to_string(),
            assertion_id: assertion_id.to_string(),
            grader_kind: result.grader.kind,
            grader_passed: result.passed,
            grader_evidence: result.evidence.clone(),
            human_verdict: verdict.verdict,
            human_rationale: verdict.rationale.clone(),
            votes: result.votes,
        });
    }
}

/// Recomputes each bucket's rate and interval from its raw counts, since
/// `record_verdict` only accumulates counts and the derived fields are cheaper to
/// recompute once at the end than to keep in sync one increment at a time.
fn finalize_bucket(bucket: AgreementBucket) -> AgreementBucket {
    AgreementBucket::from_counts(bucket.agreements, bucket.disagreements)
}

impl AgreementByGraderKind {
    fn finalized(self) -> Self {
        Self {
            mechanical: finalize_bucket(self.mechanical),
            declarative: finalize_bucket(self.declarative),
            script: finalize_bucket(self.script),
            llm: finalize_bucket(self.llm),
        }
    }
}

pub fn write_grader_agreement(report_dir: &Path, document: &GraderAgreementDocument) -> Result<()> {
    phase(phase_span!("write grader agreement"), || {
        let path = report_dir.join(GRADER_AGREEMENT_FILE_NAME);
        std::fs::write(path, serde_json::to_string_pretty(document)?)?;
        Ok(())
    })
}

#[cfg(test)]
#[path = "grader_agreement_tests.rs"]
mod tests;
