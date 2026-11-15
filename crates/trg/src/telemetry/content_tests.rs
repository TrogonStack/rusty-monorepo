use super::ContentCapture;
use crate::telemetry::env::fixed;

#[test]
fn defaults_to_no_content() {
    assert!(!ContentCapture::NoContent.includes_span());
    assert!(!ContentCapture::NoContent.includes_event());
}

#[test]
fn span_and_event_includes_both() {
    assert!(ContentCapture::SpanAndEvent.includes_span());
    assert!(ContentCapture::SpanAndEvent.includes_event());
}

#[test]
fn span_only_excludes_event() {
    assert!(ContentCapture::SpanOnly.includes_span());
    assert!(!ContentCapture::SpanOnly.includes_event());
}

#[test]
fn capture_mode_without_opt_in_stays_off() {
    let env = fixed(&[("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "SPAN_AND_EVENT")]);
    assert_eq!(ContentCapture::from_lookup(&env), ContentCapture::NoContent);
}

#[test]
fn opt_in_without_capture_mode_stays_off() {
    let env = fixed(&[("OTEL_SEMCONV_STABILITY_OPT_IN", "gen_ai_latest_experimental")]);
    assert_eq!(ContentCapture::from_lookup(&env), ContentCapture::NoContent);
}

#[test]
fn opt_in_among_others_with_capture_mode_turns_on() {
    let env = fixed(&[
        ("OTEL_SEMCONV_STABILITY_OPT_IN", "http, gen_ai_latest_experimental"),
        ("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "SPAN_ONLY"),
    ]);
    assert_eq!(ContentCapture::from_lookup(&env), ContentCapture::SpanOnly);
}

#[test]
fn unknown_capture_mode_stays_off() {
    let env = fixed(&[
        ("OTEL_SEMCONV_STABILITY_OPT_IN", "gen_ai_latest_experimental"),
        ("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "true"),
    ]);
    assert_eq!(ContentCapture::from_lookup(&env), ContentCapture::NoContent);
}
