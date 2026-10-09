use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    InvalidTopic,
    InvalidKey,
    TopicTooLong,
    KeyTooLong,
    HolderTooLong,
    MetaTooLarge,
    InvalidRequest,
    AlreadyTracked,
    Gone,
    Conflict,
    OperationConflict,
    SequenceConflict,
    RetryExpired,
    HolderLimit,
    TopicLimit,
    IdentityCapacityExceeded,
    NotReady,
    Unavailable,
    Overloaded,
    GenerationChanged,
    BarrierExpired,
    SnapshotTooLarge,
    HookRejected,
    HookUnavailable,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTopic => "invalid_topic",
            Self::InvalidKey => "invalid_key",
            Self::TopicTooLong => "topic_too_long",
            Self::KeyTooLong => "key_too_long",
            Self::HolderTooLong => "holder_too_long",
            Self::MetaTooLarge => "meta_too_large",
            Self::InvalidRequest => "invalid_request",
            Self::AlreadyTracked => "already_tracked",
            Self::Gone => "gone",
            Self::Conflict => "conflict",
            Self::OperationConflict => "operation_conflict",
            Self::SequenceConflict => "sequence_conflict",
            Self::RetryExpired => "retry_expired",
            Self::HolderLimit => "holder_limit",
            Self::TopicLimit => "topic_limit",
            Self::IdentityCapacityExceeded => "identity_capacity_exceeded",
            Self::NotReady => "not_ready",
            Self::Unavailable => "unavailable",
            Self::Overloaded => "overloaded",
            Self::GenerationChanged => "generation_changed",
            Self::BarrierExpired => "barrier_expired",
            Self::SnapshotTooLarge => "snapshot_too_large",
            Self::HookRejected => "hook_rejected",
            Self::HookUnavailable => "hook_unavailable",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::NotReady | Self::Unavailable | Self::Overloaded | Self::HookUnavailable | Self::BarrierExpired
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
