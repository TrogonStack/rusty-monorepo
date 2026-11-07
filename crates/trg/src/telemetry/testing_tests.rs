use super::capture;

#[test]
fn captures_names_attributes_and_parentage() {
    let ((), trace) = capture(|| {
        let parent = tracing::info_span!("parent", "trg.example" = "value");
        parent.in_scope(|| {
            tracing::info_span!("child").in_scope(|| ());
            tracing::info_span!("child").in_scope(|| ());
        });
    });
    let parent = trace.span("parent").expect("parent exported");
    assert_eq!(trace.children_of(parent).len(), 2);
    assert_eq!(trace.spans_named("child").len(), 2);
    assert_eq!(
        trace.attribute("parent", "trg.example").map(|v| v.to_string()),
        Some("value".to_string())
    );
}
