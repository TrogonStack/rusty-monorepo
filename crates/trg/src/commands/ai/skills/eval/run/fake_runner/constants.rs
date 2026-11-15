use std::time::Duration;

/// How long a lane waits for the lanes a test expects alongside it.
///
/// Bounded so a serial implementation fails the expectation instead of hanging on it.
pub(super) const LANE_RENDEZVOUS: Duration = Duration::from_secs(2);
