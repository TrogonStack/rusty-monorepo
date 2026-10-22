use std::time::Duration;

use async_nats::ServerInfo;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const PAYLOAD_BUDGET_MAX: usize = 256 * KIB;
const PAYLOAD_BUDGET_MIN: usize = KIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayloadBudget(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("payload budget must be between {PAYLOAD_BUDGET_MIN} and {PAYLOAD_BUDGET_MAX} bytes, got {0}")]
pub struct PayloadBudgetError(usize);

impl PayloadBudget {
    pub fn get(self) -> usize {
        self.0
    }

    pub fn for_server(self, info: &ServerInfo) -> Self {
        Self(self.0.min(info.max_payload).max(PAYLOAD_BUDGET_MIN))
    }
}

impl Default for PayloadBudget {
    fn default() -> Self {
        Self(PAYLOAD_BUDGET_MAX)
    }
}

impl TryFrom<usize> for PayloadBudget {
    type Error = PayloadBudgetError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if (PAYLOAD_BUDGET_MIN..=PAYLOAD_BUDGET_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(PayloadBudgetError(value))
        }
    }
}

macro_rules! positive_limit {
    ($name:ident, $error:ident, $inner:ty, $default:expr, $what:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($inner);

        #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
        #[error("{} must be greater than zero", $what)]
        pub struct $error;

        impl $name {
            pub fn get(self) -> $inner {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }

        impl TryFrom<$inner> for $name {
            type Error = $error;

            fn try_from(value: $inner) -> Result<Self, Self::Error> {
                if value == <$inner>::default() {
                    Err($error)
                } else {
                    Ok(Self(value))
                }
            }
        }
    };
}

positive_limit!(
    SnapshotMaxBytes,
    SnapshotMaxBytesError,
    usize,
    8 * MIB,
    "snapshot max bytes"
);
positive_limit!(SnapshotMaxParts, SnapshotMaxPartsError, u32, 512, "snapshot max parts");
positive_limit!(
    AssemblyDeadline,
    AssemblyDeadlineError,
    Duration,
    Duration::from_secs(2),
    "assembly deadline"
);
positive_limit!(
    DiffBufferBytes,
    DiffBufferBytesError,
    usize,
    4 * MIB,
    "diff buffer bytes"
);
positive_limit!(
    AssembliesPerConnection,
    AssembliesPerConnectionError,
    usize,
    2,
    "assemblies per connection"
);
positive_limit!(
    AssembliesPerProcess,
    AssembliesPerProcessError,
    usize,
    8,
    "assemblies per process"
);
positive_limit!(
    AssemblyBudgetBytes,
    AssemblyBudgetBytesError,
    usize,
    64 * MIB,
    "assembly budget bytes"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotLimits {
    payload: PayloadBudget,
    max_bytes: SnapshotMaxBytes,
    max_parts: SnapshotMaxParts,
    deadline: AssemblyDeadline,
    diff_buffer: DiffBufferBytes,
    per_connection: AssembliesPerConnection,
    per_process: AssembliesPerProcess,
    budget: AssemblyBudgetBytes,
}

impl SnapshotLimits {
    pub fn with_payload(self, payload: PayloadBudget) -> Self {
        Self { payload, ..self }
    }

    pub fn with_max_bytes(self, max_bytes: SnapshotMaxBytes) -> Self {
        Self { max_bytes, ..self }
    }

    pub fn with_max_parts(self, max_parts: SnapshotMaxParts) -> Self {
        Self { max_parts, ..self }
    }

    pub fn with_deadline(self, deadline: AssemblyDeadline) -> Self {
        Self { deadline, ..self }
    }

    pub fn with_diff_buffer(self, diff_buffer: DiffBufferBytes) -> Self {
        Self { diff_buffer, ..self }
    }

    pub fn with_per_connection(self, per_connection: AssembliesPerConnection) -> Self {
        Self { per_connection, ..self }
    }

    pub fn with_per_process(self, per_process: AssembliesPerProcess) -> Self {
        Self { per_process, ..self }
    }

    pub fn with_budget(self, budget: AssemblyBudgetBytes) -> Self {
        Self { budget, ..self }
    }

    pub fn payload(self) -> PayloadBudget {
        self.payload
    }

    pub fn max_bytes(self) -> SnapshotMaxBytes {
        self.max_bytes
    }

    pub fn max_parts(self) -> SnapshotMaxParts {
        self.max_parts
    }

    pub fn deadline(self) -> AssemblyDeadline {
        self.deadline
    }

    pub fn diff_buffer(self) -> DiffBufferBytes {
        self.diff_buffer
    }

    pub fn per_connection(self) -> AssembliesPerConnection {
        self.per_connection
    }

    pub fn per_process(self) -> AssembliesPerProcess {
        self.per_process
    }

    pub fn budget(self) -> AssemblyBudgetBytes {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_contract() {
        let limits = SnapshotLimits::default();
        assert_eq!(limits.payload().get(), 256 * KIB);
        assert_eq!(limits.max_bytes().get(), 8 * MIB);
        assert_eq!(limits.max_parts().get(), 512);
        assert_eq!(limits.deadline().get(), Duration::from_secs(2));
        assert_eq!(limits.diff_buffer().get(), 4 * MIB);
        assert_eq!(limits.per_connection().get(), 2);
        assert_eq!(limits.per_process().get(), 8);
        assert_eq!(limits.budget().get(), 64 * MIB);
        assert!(PayloadBudget::try_from(256 * KIB + 1).is_err());
        assert!(PayloadBudget::try_from(KIB - 1).is_err());
        assert!(SnapshotMaxParts::try_from(0).is_err());
        assert!(AssemblyDeadline::try_from(Duration::ZERO).is_err());
    }
}
