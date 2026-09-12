//! Which of a suite's cases one run covers.
//!
//! A suite grows, and a full pass over it costs a model call per case per scenario per
//! attempt. Running one case while iterating on it, or only the cases a change could have
//! affected, is the difference between an eval that gets run and one that does not.

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::evals::{EvalCase, EvalError, Result};

use super::validation::ValidationError;

/// A pattern a case's id is matched against, in full.
///
/// A glob rather than a regular expression, because that is what a case id looks like:
/// `analyze-*` is what an operator reaches for, and a partial match would make
/// `analyze-sales` select `analyze-sales-by-region` too.
#[derive(Debug, Clone)]
pub struct CasePattern {
    source: String,
    matcher: Regex,
}

impl CasePattern {
    pub fn parse(source: &str) -> Result<Self> {
        let source = source.trim();
        if source.is_empty() {
            return Err(selection_error("case", "a case pattern cannot be empty"));
        }
        let mut expression = String::from("^");
        for character in source.chars() {
            match character {
                '*' => expression.push_str(".*"),
                '?' => expression.push('.'),
                literal => expression.push_str(&regex::escape(literal.encode_utf8(&mut [0u8; 4]))),
            }
        }
        expression.push('$');
        let matcher = Regex::new(&expression)
            .map_err(|error| selection_error("case", format!("pattern '{source}' is not usable: {error}")))?;
        Ok(Self {
            source: source.to_string(),
            matcher,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    fn matches(&self, case_id: &str) -> bool {
        self.matcher.is_match(case_id)
    }
}

/// A tag a case declares, matched exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseTag(String);

impl CaseTag {
    pub fn parse(source: &str) -> Result<Self> {
        let source = source.trim();
        if source.is_empty() {
            return Err(selection_error("tag", "a tag cannot be empty"));
        }
        Ok(Self(source.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn carried_by(&self, case: &EvalCase) -> bool {
        case.tags
            .as_deref()
            .is_some_and(|tags| tags.iter().any(|tag| tag == &self.0))
    }
}

/// The part of a suite one run covers.
///
/// Each dimension narrows the selection, so `--case analyze-* --tag smoke` covers the
/// cases named `analyze-*` that are also tagged `smoke`. Within a dimension the values
/// are alternatives, so two `--tag` flags cover a case carrying either one.
#[derive(Debug, Clone, Default)]
pub enum CaseSelection {
    #[default]
    WholeSuite,
    Narrowed {
        patterns: Vec<CasePattern>,
        tags: Vec<CaseTag>,
    },
}

impl CaseSelection {
    pub fn parse(patterns: &[String], tags: &[String]) -> Result<Self> {
        if patterns.is_empty() && tags.is_empty() {
            return Ok(Self::WholeSuite);
        }
        Ok(Self::Narrowed {
            patterns: patterns.iter().map(|p| CasePattern::parse(p)).collect::<Result<_>>()?,
            tags: tags.iter().map(|t| CaseTag::parse(t)).collect::<Result<_>>()?,
        })
    }

    pub fn covers(&self, case: &EvalCase) -> bool {
        let Self::Narrowed { patterns, tags } = self else {
            return true;
        };
        let named = patterns.is_empty() || patterns.iter().any(|pattern| pattern.matches(case.id.as_str()));
        let tagged = tags.is_empty() || tags.iter().any(|tag| tag.carried_by(case));
        named && tagged
    }

    /// Narrow a suite's cases, refusing a selection that covers none of them.
    ///
    /// An eval run that covers nothing exits successfully with an empty report, which
    /// reads as a suite that found no problems. A mistyped pattern has to say so instead.
    pub fn apply(&self, cases: Vec<EvalCase>) -> Result<Vec<EvalCase>> {
        let Self::Narrowed { .. } = self else {
            return Ok(cases);
        };
        let of = cases.len();
        let covered: Vec<EvalCase> = cases.into_iter().filter(|case| self.covers(case)).collect();
        if covered.is_empty() {
            return Err(selection_error(
                "case",
                format!(
                    "{} matches none of the {of} cases in the suite",
                    self.describe_narrowing()
                ),
            ));
        }
        Ok(covered)
    }

    /// What a report records about a run that covered part of its suite.
    ///
    /// `declared` names the suite the selection was taken from, not just its size,
    /// because a later reader comparing two reports has to tell a case the suite lost
    /// from one this run merely did not select.
    ///
    /// A selection that ends up covering every case the suite declares covered the whole
    /// suite, however it was written, so there is no narrowing to report: the field is
    /// what tells a narrowed report from a full one, and one present on a full run says a
    /// narrowing happened that did not.
    pub fn record(&self, covered: usize, declared: Vec<String>) -> Option<CaseSelectionRecord> {
        let Self::Narrowed { patterns, tags } = self else {
            return None;
        };
        if covered == declared.len() {
            return None;
        }
        Some(CaseSelectionRecord {
            cases: patterns.iter().map(|pattern| pattern.as_str().to_string()).collect(),
            tags: tags.iter().map(|tag| tag.as_str().to_string()).collect(),
            covered,
            declared,
        })
    }

    fn describe_narrowing(&self) -> String {
        let Self::Narrowed { patterns, tags } = self else {
            return "the whole suite".to_string();
        };
        let mut parts = Vec::new();
        if !patterns.is_empty() {
            parts.push(format!(
                "--case {}",
                patterns
                    .iter()
                    .map(CasePattern::as_str)
                    .collect::<Vec<_>>()
                    .join(", --case ")
            ));
        }
        if !tags.is_empty() {
            parts.push(format!(
                "--tag {}",
                tags.iter().map(CaseTag::as_str).collect::<Vec<_>>().join(", --tag ")
            ));
        }
        parts.join(" with ")
    }
}

/// The selection a report was produced under, so a reader is not shown partial coverage
/// as if it were a pass over the whole suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaseSelectionRecord {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub covered: usize,
    /// Every case ID the suite declared, covered by this run or not.
    pub declared: Vec<String>,
}

fn selection_error(field: &str, message: impl Into<String>) -> EvalError {
    EvalError::Validation(ValidationError::for_field(field, message).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, tags: &[&str]) -> EvalCase {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "prompt": "Do the thing.",
            "expected_output": "The thing, done.",
            "tags": tags,
        }))
        .unwrap()
    }

    #[test]
    fn no_flags_covers_the_whole_suite() {
        let selection = CaseSelection::parse(&[], &[]).unwrap();
        assert!(selection.covers(&case("anything", &[])));
        assert!(selection.record(1, vec!["anything".to_string()]).is_none());
    }

    #[test]
    fn a_pattern_matches_a_case_id_in_full() {
        let selection = CaseSelection::parse(&["analyze-sales".to_string()], &[]).unwrap();
        assert!(selection.covers(&case("analyze-sales", &[])));
        assert!(
            !selection.covers(&case("analyze-sales-by-region", &[])),
            "a partial match would select cases the operator did not name"
        );
    }

    #[test]
    fn a_glob_stands_for_the_part_of_an_id_that_varies() {
        let selection = CaseSelection::parse(&["analyze-*".to_string()], &[]).unwrap();
        assert!(selection.covers(&case("analyze-sales", &[])));
        assert!(selection.covers(&case("analyze-", &[])));
        assert!(!selection.covers(&case("summarize-sales", &[])));
    }

    #[test]
    fn a_regular_expression_is_read_as_the_literal_id_it_is_not() {
        let selection = CaseSelection::parse(&["analyze.sales".to_string()], &[]).unwrap();
        assert!(selection.covers(&case("analyze.sales", &[])));
        assert!(!selection.covers(&case("analyze-sales", &[])));
    }

    #[test]
    fn each_dimension_narrows_and_each_value_within_one_widens() {
        let selection = CaseSelection::parse(
            &["analyze-*".to_string()],
            &["smoke".to_string(), "regression".to_string()],
        )
        .unwrap();

        assert!(selection.covers(&case("analyze-sales", &["regression"])));
        assert!(!selection.covers(&case("analyze-sales", &["slow"])));
        assert!(!selection.covers(&case("summarize-sales", &["smoke"])));
    }

    #[test]
    fn an_untagged_case_carries_no_tag_to_select_it_by() {
        let selection = CaseSelection::parse(&[], &["smoke".to_string()]).unwrap();
        let untagged: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "analyze-sales",
            "prompt": "Do the thing.",
            "expected_output": "The thing, done.",
        }))
        .unwrap();

        assert!(!selection.covers(&untagged));
    }

    #[test]
    fn a_selection_that_covers_nothing_is_refused() {
        let selection = CaseSelection::parse(&["typo-*".to_string()], &[]).unwrap();
        let error = selection.apply(vec![case("analyze-sales", &[])]).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("--case typo-*"), "{message}");
        assert!(message.contains("none of the 1 cases"), "{message}");
    }

    #[test]
    fn an_empty_pattern_is_refused_rather_than_matching_nothing() {
        assert!(CaseSelection::parse(&["   ".to_string()], &[]).is_err());
        assert!(CaseSelection::parse(&[], &["".to_string()]).is_err());
    }

    /// The field is what tells a narrowed report from a full one, so a pattern that
    /// happens to match every case the suite declares has no narrowing to record.
    #[test]
    fn a_selection_that_matched_every_case_records_no_narrowing() {
        let selection = CaseSelection::parse(&["analyze-*".to_string()], &[]).unwrap();
        let declared = vec!["analyze-sales".to_string(), "analyze-refunds".to_string()];

        assert!(selection.record(declared.len(), declared).is_none());
    }

    #[test]
    fn a_narrowed_run_records_the_suite_it_selected_from() {
        let selection = CaseSelection::parse(&["analyze-*".to_string()], &["smoke".to_string()]).unwrap();
        let declared = vec!["analyze-sales".to_string(), "typo-check".to_string()];
        let record = selection.record(1, declared.clone()).unwrap();

        assert_eq!(record.cases, vec!["analyze-*"]);
        assert_eq!(record.tags, vec!["smoke"]);
        assert_eq!(record.covered, 1);
        assert_eq!(record.declared, declared);
    }
}
