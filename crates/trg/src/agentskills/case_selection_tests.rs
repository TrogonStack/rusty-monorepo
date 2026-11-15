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

fn case_with_split(id: &str, split: EvalSplit) -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "prompt": "Do the thing.",
        "expected_output": "The thing, done.",
        "split": split.as_str(),
    }))
    .unwrap()
}

fn declared(id: &str, split: EvalSplit) -> DeclaredEvalCase {
    DeclaredEvalCase {
        id: id.to_string(),
        split: Some(split),
    }
}

#[test]
fn no_flags_covers_the_whole_suite() {
    let selection = CaseSelection::parse(&[], &[], None).unwrap();
    assert!(selection.covers(&case("anything", &[])));
    assert!(selection
        .record(1, vec![declared("anything", EvalSplit::Train)])
        .is_none());
}

#[test]
fn a_pattern_matches_a_case_id_in_full() {
    let selection = CaseSelection::parse(&["analyze-sales".to_string()], &[], None).unwrap();
    assert!(selection.covers(&case("analyze-sales", &[])));
    assert!(
        !selection.covers(&case("analyze-sales-by-region", &[])),
        "a partial match would select cases the operator did not name"
    );
}

#[test]
fn a_glob_stands_for_the_part_of_an_id_that_varies() {
    let selection = CaseSelection::parse(&["analyze-*".to_string()], &[], None).unwrap();
    assert!(selection.covers(&case("analyze-sales", &[])));
    assert!(selection.covers(&case("analyze-", &[])));
    assert!(!selection.covers(&case("summarize-sales", &[])));
}

#[test]
fn a_regular_expression_is_read_as_the_literal_id_it_is_not() {
    let selection = CaseSelection::parse(&["analyze.sales".to_string()], &[], None).unwrap();
    assert!(selection.covers(&case("analyze.sales", &[])));
    assert!(!selection.covers(&case("analyze-sales", &[])));
}

#[test]
fn each_dimension_narrows_and_each_value_within_one_widens() {
    let selection = CaseSelection::parse(
        &["analyze-*".to_string()],
        &["smoke".to_string(), "regression".to_string()],
        None,
    )
    .unwrap();

    assert!(selection.covers(&case("analyze-sales", &["regression"])));
    assert!(!selection.covers(&case("analyze-sales", &["slow"])));
    assert!(!selection.covers(&case("summarize-sales", &["smoke"])));
}

#[test]
fn an_untagged_case_carries_no_tag_to_select_it_by() {
    let selection = CaseSelection::parse(&[], &["smoke".to_string()], None).unwrap();
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
    let selection = CaseSelection::parse(&["typo-*".to_string()], &[], None).unwrap();
    let error = selection.apply(vec![case("analyze-sales", &[])]).unwrap_err();

    let message = error.to_string();
    assert!(message.contains("--case typo-*"), "{message}");
    assert!(message.contains("none of the 1 cases"), "{message}");
}

#[test]
fn an_empty_pattern_is_refused_rather_than_matching_nothing() {
    assert!(CaseSelection::parse(&["   ".to_string()], &[], None).is_err());
    assert!(CaseSelection::parse(&[], &["".to_string()], None).is_err());
}

/// The field is what tells a narrowed report from a full one, so a pattern that
/// happens to match every case the suite declares has no narrowing to record.
#[test]
fn a_selection_that_matched_every_case_records_no_narrowing() {
    let selection = CaseSelection::parse(&["analyze-*".to_string()], &[], None).unwrap();
    let declared_cases = vec![
        declared("analyze-sales", EvalSplit::Train),
        declared("analyze-refunds", EvalSplit::Train),
    ];

    assert!(selection.record(declared_cases.len(), declared_cases).is_none());
}

#[test]
fn a_narrowed_run_records_the_suite_it_selected_from() {
    let selection = CaseSelection::parse(&["analyze-*".to_string()], &["smoke".to_string()], None).unwrap();
    let declared_cases = vec![
        declared("analyze-sales", EvalSplit::Train),
        declared("typo-check", EvalSplit::Test),
    ];
    let record = selection.record(1, declared_cases.clone()).unwrap();

    assert_eq!(record.cases, vec!["analyze-*"]);
    assert_eq!(record.tags, vec!["smoke"]);
    assert_eq!(record.split, None);
    assert_eq!(record.covered, 1);
    assert_eq!(record.declared, declared_cases);
}

#[test]
fn split_narrows_the_same_way_case_and_tag_do() {
    let selection = CaseSelection::parse(&[], &[], Some(EvalSplit::Test)).unwrap();

    assert!(selection.covers(&case_with_split("analyze-sales", EvalSplit::Test)));
    assert!(!selection.covers(&case_with_split("analyze-sales", EvalSplit::Train)));
}

#[test]
fn split_composes_with_case_and_tag_as_a_third_narrowing_dimension() {
    let selection = CaseSelection::parse(
        &["analyze-*".to_string()],
        &["smoke".to_string()],
        Some(EvalSplit::Test),
    )
    .unwrap();

    let mut matching = case_with_split("analyze-sales", EvalSplit::Test);
    matching.tags = Some(vec!["smoke".to_string()]);
    assert!(selection.covers(&matching));

    let mut wrong_split = matching.clone();
    wrong_split.split = EvalSplit::Train;
    assert!(!selection.covers(&wrong_split));
}

#[test]
fn a_narrowed_run_records_the_split_it_selected_by() {
    let selection = CaseSelection::parse(&[], &[], Some(EvalSplit::Test)).unwrap();
    let declared_cases = vec![
        declared("analyze-sales", EvalSplit::Test),
        declared("analyze-refunds", EvalSplit::Test),
    ];
    let record = selection.record(1, declared_cases).unwrap();

    assert_eq!(record.split, Some(EvalSplit::Test));
}

#[test]
fn an_older_reports_legacy_string_declared_list_deserializes_with_an_unknown_split() {
    let record: CaseSelectionRecord = serde_json::from_value(serde_json::json!({
        "covered": 1,
        "declared": ["analyze-sales", "analyze-refunds"]
    }))
    .expect("a legacy bare-string declared list must still deserialize");

    assert_eq!(
        record.declared,
        vec![
            DeclaredEvalCase {
                id: "analyze-sales".to_string(),
                split: None,
            },
            DeclaredEvalCase {
                id: "analyze-refunds".to_string(),
                split: None,
            },
        ],
        "a legacy entry carries no split of its own, so it must deserialize as unknown \
             rather than being guessed as train or test"
    );
}

#[test]
fn a_rewritten_legacy_declared_list_still_deserializes_with_an_unknown_split() {
    let legacy: CaseSelectionRecord = serde_json::from_value(serde_json::json!({
        "covered": 1,
        "declared": ["analyze-sales"]
    }))
    .unwrap();

    let rewritten = serde_json::to_value(&legacy).unwrap();
    let reread: CaseSelectionRecord = serde_json::from_value(rewritten)
        .expect("a report rewritten after loading a legacy declared list must stay readable");

    assert_eq!(reread, legacy);
}
