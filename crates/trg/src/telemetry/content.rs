//! Whether GenAI message content may be captured on spans and/or log events.
//!
//! Follows the OpenTelemetry Python GenAI contrib convention: capture is off
//! unless both `OTEL_SEMCONV_STABILITY_OPT_IN` contains
//! `gen_ai_latest_experimental` and `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`
//! names a capture mode. A typed enum rather than two booleans, since "span
//! only" and "event only" are both valid and not independent choices.

use super::env::{EnvLookup, ProcessEnv};

/// Where (if anywhere) GenAI message content may be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentCapture {
    /// Never capture message content.
    NoContent,
    /// Capture on span attributes only.
    SpanOnly,
    /// Capture on log events only.
    EventOnly,
    /// Capture on both span attributes and log events.
    SpanAndEvent,
}

impl ContentCapture {
    /// Reads `OTEL_SEMCONV_STABILITY_OPT_IN` and
    /// `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` from the process
    /// environment. Defaults to [`ContentCapture::NoContent`] unless both are
    /// set correctly, so content capture never turns on by accident.
    pub fn from_env() -> Self {
        Self::from_lookup(&ProcessEnv)
    }

    /// [`ContentCapture::from_env`] against any variable source.
    pub fn from_lookup(env: &impl EnvLookup) -> Self {
        if !Self::opted_into_latest_experimental(env) {
            return Self::NoContent;
        }
        match env.get("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT").as_deref() {
            Some("SPAN_ONLY") => Self::SpanOnly,
            Some("EVENT_ONLY") => Self::EventOnly,
            Some("SPAN_AND_EVENT") => Self::SpanAndEvent,
            _ => Self::NoContent,
        }
    }

    fn opted_into_latest_experimental(env: &impl EnvLookup) -> bool {
        env.get("OTEL_SEMCONV_STABILITY_OPT_IN")
            .is_some_and(|v| v.split(',').any(|value| value.trim() == "gen_ai_latest_experimental"))
    }

    pub fn includes_span(self) -> bool {
        matches!(self, Self::SpanOnly | Self::SpanAndEvent)
    }

    pub fn includes_event(self) -> bool {
        matches!(self, Self::EventOnly | Self::SpanAndEvent)
    }
}

#[cfg(test)]
mod tests {
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
}
