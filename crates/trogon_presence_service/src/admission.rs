use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use trogon_presence::{RateBurst, RateLimit};

const DEFAULT_PROCESS_QUEUE_DEPTH: usize = 256;
const DEFAULT_KEY_QUEUE_DEPTH: usize = 8;
const DEFAULT_KEY_REFILL: Duration = Duration::from_millis(50);
const DEFAULT_KEY_BURST: u32 = 50;
const DEFAULT_NODE_REFILL: Duration = Duration::from_millis(100);
const DEFAULT_NODE_BURST: u32 = 20;

macro_rules! queue_depth {
    ($name:ident, $error:ident, $default:expr, $what:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(usize);

        #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
        #[error("{} must be at least 1", $what)]
        pub struct $error;

        impl $name {
            pub fn get(self) -> usize {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }

        impl TryFrom<usize> for $name {
            type Error = $error;

            fn try_from(value: usize) -> Result<Self, Self::Error> {
                if value == 0 {
                    Err($error)
                } else {
                    Ok(Self(value))
                }
            }
        }
    };
}

queue_depth!(
    ProcessQueueDepth,
    ProcessQueueDepthError,
    DEFAULT_PROCESS_QUEUE_DEPTH,
    "process queue depth"
);
queue_depth!(
    KeyQueueDepth,
    KeyQueueDepthError,
    DEFAULT_KEY_QUEUE_DEPTH,
    "key queue depth"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimits {
    process: ProcessQueueDepth,
    key: KeyQueueDepth,
    key_rate: RateLimit,
    node_rate: RateLimit,
}

impl AdmissionLimits {
    pub fn with_process(self, process: ProcessQueueDepth) -> Self {
        Self { process, ..self }
    }

    pub fn with_key(self, key: KeyQueueDepth) -> Self {
        Self { key, ..self }
    }

    pub fn with_key_rate(self, key_rate: RateLimit) -> Self {
        Self { key_rate, ..self }
    }

    pub fn with_node_rate(self, node_rate: RateLimit) -> Self {
        Self { node_rate, ..self }
    }

    pub fn process(self) -> ProcessQueueDepth {
        self.process
    }

    pub fn key(self) -> KeyQueueDepth {
        self.key
    }

    pub fn key_rate(self) -> RateLimit {
        self.key_rate
    }

    pub fn node_rate(self) -> RateLimit {
        self.node_rate
    }
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            process: ProcessQueueDepth::default(),
            key: KeyQueueDepth::default(),
            key_rate: rate(DEFAULT_KEY_REFILL, DEFAULT_KEY_BURST),
            node_rate: rate(DEFAULT_NODE_REFILL, DEFAULT_NODE_BURST),
        }
    }
}

fn rate(refill: Duration, burst: u32) -> RateLimit {
    RateBurst::try_from(burst)
        .and_then(|burst| RateLimit::new(refill, burst))
        .unwrap_or_default()
}

#[derive(Debug, Default)]
pub(crate) struct AdmissionCounters {
    forwarded: AtomicU64,
    admitted: AtomicU64,
    rejected_inbox: AtomicU64,
    rate_limited: AtomicU64,
    overloaded: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counter {
    Forwarded,
    Admitted,
    RejectedInbox,
    RateLimited,
    Overloaded,
}

impl AdmissionCounters {
    pub(crate) fn bump(&self, counter: Counter) {
        let cell = match counter {
            Counter::Forwarded => &self.forwarded,
            Counter::Admitted => &self.admitted,
            Counter::RejectedInbox => &self.rejected_inbox,
            Counter::RateLimited => &self.rate_limited,
            Counter::Overloaded => &self.overloaded,
        };
        cell.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> AdmissionStats {
        AdmissionStats {
            forwarded: self.forwarded.load(Ordering::Relaxed),
            admitted: self.admitted.load(Ordering::Relaxed),
            rejected_inbox: self.rejected_inbox.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            overloaded: self.overloaded.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionStats {
    forwarded: u64,
    admitted: u64,
    rejected_inbox: u64,
    rate_limited: u64,
    overloaded: u64,
}

impl AdmissionStats {
    pub fn forwarded(self) -> u64 {
        self.forwarded
    }

    pub fn admitted(self) -> u64 {
        self.admitted
    }

    pub fn rejected_inbox(self) -> u64 {
        self.rejected_inbox
    }

    pub fn rate_limited(self) -> u64 {
        self.rate_limited
    }

    pub fn overloaded(self) -> u64 {
        self.overloaded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let limits = AdmissionLimits::default();
        assert_eq!(limits.process().get(), 256);
        assert_eq!(limits.key().get(), 8);
        assert_eq!(limits.key_rate().refill_interval(), Duration::from_millis(50));
        assert_eq!(limits.key_rate().burst().get(), 50);
        assert!(ProcessQueueDepth::try_from(0).is_err());
        assert!(KeyQueueDepth::try_from(0).is_err());
    }
}
