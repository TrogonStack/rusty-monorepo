use super::Duration;
use crate::telemetry::INTERRUPT_FLUSH_TIMEOUT;

/// How long a group is given to exit on its own before it is killed outright.
///
/// Long enough for a harness to finish flushing the transcript it was in the middle of
/// writing, which is the artifact the timed-out run is diagnosed from.
pub const TERMINATION_GRACE: Duration = Duration::from_millis(500);

/// Process groups currently running a harness.
///
/// A fixed array of atomics rather than a lock: the termination handler runs in signal
/// context, where taking a lock the interrupted thread already holds deadlocks the
/// process. Slot value `0` means free.
pub const MAX_TRACKED_GROUPS: usize = 64;

/// How long the handler holds the signal back for the watcher before re-raising it
/// regardless, so an unreachable collector or a lock the interrupted thread holds can
/// delay the exit but never prevent it.
pub const FLUSH_WAIT: Duration = Duration::from_millis(INTERRUPT_FLUSH_TIMEOUT.as_millis() as u64 + 500);
