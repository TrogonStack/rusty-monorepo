use std::time::Duration;

pub const CALLOUT_SPAN: &str = "presence.auth.callout";
pub const REVOKE_SPAN: &str = "presence.auth.revoke";
pub const DECISIONS: &str = "trogon.presence.auth.decisions";
pub const DURATION: &str = "trogon.presence.auth.duration";
pub const JWT_SUBJECTS: &str = "trogon.presence.auth.jwt.subjects";
pub const REVOCATIONS: &str = "trogon.presence.auth.revocations";
pub const OUTCOME_ATTRIBUTE: &str = "presence.auth.outcome";
pub const ERROR_TYPE_ATTRIBUTE: &str = "error.type";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allowed,
    Denied,
    RateLimited,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::RateLimited => "rate_limited",
        }
    }
}

pub fn record_decision(outcome: Outcome, error_type: Option<&'static str>, elapsed: Duration) {
    metrics::counter!(
        DECISIONS,
        OUTCOME_ATTRIBUTE => outcome.as_str(),
        ERROR_TYPE_ATTRIBUTE => error_type.unwrap_or("")
    )
    .increment(1);
    metrics::histogram!(DURATION, OUTCOME_ATTRIBUTE => outcome.as_str()).record(elapsed.as_secs_f64());
}

pub fn record_subjects(count: usize) {
    metrics::histogram!(JWT_SUBJECTS).record(count as f64);
}

pub fn record_revocation() {
    metrics::counter!(REVOCATIONS).increment(1);
}
