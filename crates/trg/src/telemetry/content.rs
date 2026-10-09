//! Whether GenAI message content may be captured on spans and/or log events.
//!
//! Follows the OpenTelemetry Python GenAI contrib convention: capture is off
//! unless both `OTEL_SEMCONV_STABILITY_OPT_IN` contains
//! `gen_ai_latest_experimental` and `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`
//! names a capture mode. A typed enum rather than two booleans, since "span
//! only" and "event only" are both valid and not independent choices.

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
        if !Self::opted_into_latest_experimental() {
            return Self::NoContent;
        }
        match std::env::var("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT")
            .ok()
            .as_deref()
        {
            Some("SPAN_ONLY") => Self::SpanOnly,
            Some("EVENT_ONLY") => Self::EventOnly,
            Some("SPAN_AND_EVENT") => Self::SpanAndEvent,
            _ => Self::NoContent,
        }
    }

    fn opted_into_latest_experimental() -> bool {
        std::env::var("OTEL_SEMCONV_STABILITY_OPT_IN")
            .map(|v| v.split(',').any(|value| value.trim() == "gen_ai_latest_experimental"))
            .unwrap_or(false)
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
}
