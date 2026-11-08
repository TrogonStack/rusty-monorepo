use super::*;

fn call<'a>(tool: &'a str, arguments: Option<&'a str>, result: Option<&'a str>) -> ObservedToolCall<'a> {
    ObservedToolCall {
        tool,
        arguments,
        result,
    }
}

#[test]
fn counts_calls_per_tool_and_keeps_the_largest_result() {
    let metrics = TranscriptMetrics::measure(
        TurnCount::new(3),
        Some(1200),
        [
            call("Read", Some(r#"{"path":"a"}"#), Some("12345")),
            call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
            call("Read", Some(r#"{"path":"b"}"#), Some("123456789")),
        ],
    );

    assert_eq!(metrics.turns, TurnCount::new(3));
    assert_eq!(metrics.tool_calls, ToolCallCount::new(3));
    assert_eq!(metrics.repeated_tool_calls, ToolCallCount::new(0));
    assert_eq!(metrics.peak_turn_input_tokens, Some(1200));
    let names: Vec<&str> = metrics.tools.iter().map(|tool| tool.tool.as_str()).collect();
    assert_eq!(names, ["Grep", "Read"]);
    assert_eq!(metrics.tools[1].calls, ToolCallCount::new(2));
    assert_eq!(metrics.tools[1].largest_result_bytes, Some(ResultBytes::new(9)));
}

#[test]
fn a_repeated_call_is_the_same_tool_with_the_same_arguments() {
    let metrics = TranscriptMetrics::measure(
        TurnCount::new(4),
        None,
        [
            call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
            call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
            call("Glob", Some(r#"{"q":"x"}"#), None),
            call("Grep", Some(r#"{"q":"y"}"#), None),
            call("Grep", None, None),
            call("Grep", None, None),
        ],
    );

    assert_eq!(metrics.repeated_tool_calls, ToolCallCount::new(1));
    let grep = metrics.tools.iter().find(|tool| tool.tool == "Grep").unwrap();
    assert_eq!(grep.repeated_calls, ToolCallCount::new(1));
    let glob = metrics.tools.iter().find(|tool| tool.tool == "Glob").unwrap();
    assert_eq!(glob.largest_result_bytes, None);
}

#[test]
fn tallies_tools_across_runs() {
    let first = TranscriptMetrics::measure(
        TurnCount::new(1),
        None,
        [call("Read", Some("a"), Some("12")), call("Read", Some("a"), Some("1"))],
    );
    let second = TranscriptMetrics::measure(TurnCount::new(1), None, [call("Read", Some("b"), Some("1234"))]);

    let tallies = tally_tools([&first, &second]);

    assert_eq!(
        tallies,
        [ToolTally {
            tool: "Read".to_string(),
            runs: RunCount::new(2),
            calls: ToolCallCount::new(3),
            repeated_calls: ToolCallCount::new(1),
            largest_result_bytes: Some(ResultBytes::new(4)),
        }]
    );
}

#[test]
fn round_trips_through_json_without_empty_fields() {
    let metrics = TranscriptMetrics::measure(TurnCount::new(2), None, []);

    let json = serde_json::to_value(&metrics).unwrap();

    assert_eq!(
        json,
        serde_json::json!({"turns": 2, "tool_calls": 0, "repeated_tool_calls": 0})
    );
    assert_eq!(serde_json::from_value::<TranscriptMetrics>(json).unwrap(), metrics);
}
