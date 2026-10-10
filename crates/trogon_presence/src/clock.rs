//! Suspend-aware owner clock and the self-fence built on it.
//!
//! Owner elapsed-time checks read a monotonic clock that keeps counting while the host is
//! suspended (Linux `CLOCK_BOOTTIME`, macOS `mach_continuous_time`) next to one that stops
//! (`CLOCK_MONOTONIC`, `mach_absolute_time`). The difference between the two reveals a
//! suspend, and any reading that runs backwards is reported instead of being clamped.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Largest gap between the continuous and the awake clock that still counts as steady.
pub const SUSPEND_TOLERANCE: Duration = Duration::from_millis(100);

/// One reading of the suspend-aware clock pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspendAwareInstant {
    continuous: Duration,
    awake: Duration,
}

impl SuspendAwareInstant {
    /// Builds a reading from a clock that counts suspend and one that does not.
    pub fn new(continuous: Duration, awake: Duration) -> Self {
        Self { continuous, awake }
    }

    pub fn continuous(self) -> Duration {
        self.continuous
    }

    pub fn awake(self) -> Duration {
        self.awake
    }

    /// Measures how much time passed between this reading and a later one.
    pub fn elapsed_until(self, later: SuspendAwareInstant) -> Elapsed {
        let (Some(total), Some(awake)) = (
            later.continuous.checked_sub(self.continuous),
            later.awake.checked_sub(self.awake),
        ) else {
            let by = self
                .continuous
                .saturating_sub(later.continuous)
                .max(self.awake.saturating_sub(later.awake));
            return Elapsed::Backward { by };
        };
        match total.checked_sub(awake) {
            None if awake - total > SUSPEND_TOLERANCE => Elapsed::Backward { by: awake - total },
            None => Elapsed::Steady(total),
            Some(asleep) if asleep > SUSPEND_TOLERANCE => Elapsed::Suspended { elapsed: total, asleep },
            Some(_) => Elapsed::Steady(total),
        }
    }
}

/// Outcome of comparing two suspend-aware readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elapsed {
    Steady(Duration),
    Suspended { elapsed: Duration, asleep: Duration },
    Backward { by: Duration },
}

/// A source of suspend-aware readings; tests substitute a controlled one.
pub trait ClockSource: Send + Sync + fmt::Debug {
    fn now(&self) -> SuspendAwareInstant;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClockError {
    #[error("no suspend-aware clock is validated on {0}, clustered ownership cannot start")]
    UnsupportedPlatform(&'static str),
    #[error("the platform clock {0} is unavailable, clustered ownership cannot start")]
    Unavailable(&'static str),
}

#[derive(Debug, Clone)]
pub struct SuspendAwareClock(Arc<dyn ClockSource>);

impl SuspendAwareClock {
    /// Opens the platform suspend-aware clock, refusing platforms without one.
    pub fn system() -> Result<Self, ClockError> {
        Ok(Self(Arc::new(platform::SystemSource::open()?)))
    }

    pub fn from_source(source: Arc<dyn ClockSource>) -> Self {
        Self(source)
    }

    /// A clock that only moves when the returned control moves it.
    pub fn controlled() -> (Self, ClockControl) {
        let control = ClockControl::default();
        (Self(Arc::new(control.clone())), control)
    }

    pub fn now(&self) -> SuspendAwareInstant {
        self.0.now()
    }

    pub fn elapsed(&self, since: SuspendAwareInstant) -> Elapsed {
        since.elapsed_until(self.now())
    }
}

/// Moves a controlled clock: plain advance, a suspend gap, or a backward step.
#[derive(Debug, Clone)]
pub struct ClockControl(Arc<Mutex<SuspendAwareInstant>>);

/// Where a controlled clock starts, far enough from zero that a backward step stays visible.
const CONTROLLED_ORIGIN: Duration = Duration::from_secs(24 * 60 * 60);

impl Default for ClockControl {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(SuspendAwareInstant::new(
            CONTROLLED_ORIGIN,
            CONTROLLED_ORIGIN,
        ))))
    }
}

impl Default for SuspendAwareInstant {
    fn default() -> Self {
        Self::new(Duration::ZERO, Duration::ZERO)
    }
}

impl ClockControl {
    pub fn advance(&self, by: Duration) {
        self.update(|now| SuspendAwareInstant::new(now.continuous + by, now.awake + by));
    }

    pub fn suspend(&self, asleep: Duration) {
        self.update(|now| SuspendAwareInstant::new(now.continuous + asleep, now.awake));
    }

    pub fn step_back(&self, by: Duration) {
        self.update(|now| SuspendAwareInstant::new(now.continuous.saturating_sub(by), now.awake.saturating_sub(by)));
    }

    fn update(&self, step: impl FnOnce(SuspendAwareInstant) -> SuspendAwareInstant) {
        let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *now = step(*now);
    }
}

impl ClockSource for ClockControl {
    fn now(&self) -> SuspendAwareInstant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How long ownership stays valid after the send time of its last confirmed renewal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SelfFenceBound(Duration);

impl SelfFenceBound {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl From<Duration> for SelfFenceBound {
    fn from(bound: Duration) -> Self {
        Self(bound)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FenceBreach {
    #[error("ownership expired: {elapsed:?} passed against a self-fence of {bound:?}")]
    Expired { elapsed: Duration, bound: Duration },
    #[error("the host was suspended for {asleep:?} while owning")]
    Suspended { asleep: Duration },
    #[error("the owner clock moved backwards by {by:?}")]
    Backward { by: Duration },
}

/// Cached ownership: valid while the clock stays steady and within the bound of the
/// send time of the last accepted acquisition or renewal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfFence {
    confirmed: SuspendAwareInstant,
    bound: SelfFenceBound,
}

impl SelfFence {
    /// Accepts an acquisition or renewal sent at `sent` and acknowledged by `now`.
    pub fn confirm(
        sent: SuspendAwareInstant,
        now: SuspendAwareInstant,
        bound: SelfFenceBound,
    ) -> Result<Self, FenceBreach> {
        let fence = Self { confirmed: sent, bound };
        fence.remaining(now)?;
        Ok(fence)
    }

    /// Time left before the self-fence, or why cached ownership is no longer valid.
    pub fn remaining(&self, now: SuspendAwareInstant) -> Result<Duration, FenceBreach> {
        match self.confirmed.elapsed_until(now) {
            Elapsed::Steady(elapsed) if elapsed < self.bound.0 => Ok(self.bound.0 - elapsed),
            Elapsed::Steady(elapsed) => Err(FenceBreach::Expired {
                elapsed,
                bound: self.bound.0,
            }),
            Elapsed::Suspended { asleep, .. } => Err(FenceBreach::Suspended { asleep }),
            Elapsed::Backward { by } => Err(FenceBreach::Backward { by }),
        }
    }

    /// Accepts a renewal only when the current tenure is still valid at `now` and the
    /// renewal itself was sent after the last confirmation and acknowledged within bound.
    pub fn renew(&self, sent: SuspendAwareInstant, now: SuspendAwareInstant) -> Result<Self, FenceBreach> {
        self.remaining(now)?;
        if let Elapsed::Backward { by } = self.confirmed.elapsed_until(sent) {
            return Err(FenceBreach::Backward { by });
        }
        Self::confirm(sent, now, self.bound)
    }

    /// The breach to report once a timer armed for the remaining tenure fires. Timers run on
    /// the awake clock, so a fence that still reads as valid is reported as expired at its bound.
    pub fn lapse(&self, now: SuspendAwareInstant) -> FenceBreach {
        match self.remaining(now) {
            Err(breach) => breach,
            Ok(_) => FenceBreach::Expired {
                elapsed: self.bound.0,
                bound: self.bound.0,
            },
        }
    }

    pub fn confirmed_at(&self) -> SuspendAwareInstant {
        self.confirmed
    }

    pub fn bound(&self) -> SelfFenceBound {
        self.bound
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::time::Duration;

    use super::{ClockError, ClockSource, SuspendAwareInstant};

    #[repr(C)]
    #[derive(Default)]
    struct MachTimebase {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebase) -> i32;
    }

    #[derive(Debug)]
    pub(super) struct SystemSource {
        numer: u128,
        denom: u128,
    }

    impl SystemSource {
        pub(super) fn open() -> Result<Self, ClockError> {
            let mut timebase = MachTimebase::default();
            let status = unsafe { mach_timebase_info(&mut timebase) };
            if status != 0 || timebase.numer == 0 || timebase.denom == 0 {
                return Err(ClockError::Unavailable("mach_timebase_info"));
            }
            Ok(Self {
                numer: u128::from(timebase.numer),
                denom: u128::from(timebase.denom),
            })
        }

        fn convert(&self, ticks: u64) -> Duration {
            let nanos = u128::from(ticks) * self.numer / self.denom;
            Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
        }
    }

    impl ClockSource for SystemSource {
        fn now(&self) -> SuspendAwareInstant {
            let (continuous, awake) = unsafe { (mach_continuous_time(), mach_absolute_time()) };
            SuspendAwareInstant::new(self.convert(continuous), self.convert(awake))
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use std::time::Duration;

    use super::{ClockError, ClockSource, SuspendAwareInstant};

    #[derive(Debug)]
    pub(super) struct SystemSource;

    fn read(clock: libc::clockid_t) -> Option<Duration> {
        let mut spec = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let status = unsafe { libc::clock_gettime(clock, &mut spec) };
        if status != 0 {
            return None;
        }
        let secs = u64::try_from(spec.tv_sec).ok()?;
        let nanos = u32::try_from(spec.tv_nsec).ok()?;
        Some(Duration::new(secs, nanos))
    }

    impl SystemSource {
        pub(super) fn open() -> Result<Self, ClockError> {
            read(libc::CLOCK_BOOTTIME).ok_or(ClockError::Unavailable("CLOCK_BOOTTIME"))?;
            read(libc::CLOCK_MONOTONIC).ok_or(ClockError::Unavailable("CLOCK_MONOTONIC"))?;
            Ok(Self)
        }
    }

    impl ClockSource for SystemSource {
        fn now(&self) -> SuspendAwareInstant {
            let continuous = read(libc::CLOCK_BOOTTIME).unwrap_or_default();
            let awake = read(libc::CLOCK_MONOTONIC).unwrap_or_default();
            SuspendAwareInstant::new(continuous, awake)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
mod platform {
    use super::{ClockError, ClockSource, SuspendAwareInstant};

    #[derive(Debug)]
    pub(super) enum SystemSource {}

    impl SystemSource {
        pub(super) fn open() -> Result<Self, ClockError> {
            Err(ClockError::UnsupportedPlatform(std::env::consts::OS))
        }
    }

    impl ClockSource for SystemSource {
        fn now(&self) -> SuspendAwareInstant {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUND: Duration = Duration::from_secs(8);

    fn fence(clock: &SuspendAwareClock) -> Result<SelfFence, FenceBreach> {
        let sent = clock.now();
        SelfFence::confirm(sent, clock.now(), SelfFenceBound::from(BOUND))
    }

    #[test]
    fn the_system_clock_is_steady_between_two_reads() -> Result<(), ClockError> {
        let clock = SuspendAwareClock::system()?;
        let first = clock.now();
        std::thread::sleep(Duration::from_millis(5));
        match clock.elapsed(first) {
            Elapsed::Steady(elapsed) => assert!(elapsed >= Duration::from_millis(5)),
            other => panic!("expected a steady reading, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn renewal_within_the_bound_is_accepted() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        let tenure = fence(&clock)?;
        control.advance(Duration::from_secs(5));
        let sent = clock.now();
        control.advance(Duration::from_millis(200));
        let renewed = tenure.renew(sent, clock.now())?;
        assert_eq!(renewed.confirmed_at(), sent);
        control.advance(Duration::from_secs(7));
        assert_eq!(renewed.remaining(clock.now())?, Duration::from_millis(800));
        Ok(())
    }

    #[test]
    fn renewal_after_a_suspend_is_rejected() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        let tenure = fence(&clock)?;
        control.advance(Duration::from_secs(1));
        let sent = clock.now();
        control.suspend(Duration::from_secs(2));
        let rejected = tenure.renew(sent, clock.now());
        assert_eq!(
            rejected,
            Err(FenceBreach::Suspended {
                asleep: Duration::from_secs(2)
            })
        );
        Ok(())
    }

    #[test]
    fn a_short_suspend_inside_the_bound_still_invalidates() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        let tenure = fence(&clock)?;
        control.suspend(Duration::from_millis(500));
        control.advance(Duration::from_secs(1));
        assert!(matches!(
            tenure.remaining(clock.now()),
            Err(FenceBreach::Suspended { .. })
        ));
        Ok(())
    }

    #[test]
    fn ownership_expires_at_the_bound() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        let tenure = fence(&clock)?;
        control.advance(BOUND - Duration::from_millis(1));
        assert_eq!(tenure.remaining(clock.now())?, Duration::from_millis(1));
        control.advance(Duration::from_millis(1));
        assert_eq!(
            tenure.remaining(clock.now()),
            Err(FenceBreach::Expired {
                elapsed: BOUND,
                bound: BOUND
            })
        );
        Ok(())
    }

    #[test]
    fn a_backward_step_invalidates() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        control.advance(Duration::from_secs(10));
        let tenure = fence(&clock)?;
        control.step_back(Duration::from_secs(1));
        assert_eq!(
            tenure.remaining(clock.now()),
            Err(FenceBreach::Backward {
                by: Duration::from_secs(1)
            })
        );
        Ok(())
    }

    #[test]
    fn a_renewal_sent_before_the_last_confirmation_is_rejected() -> Result<(), FenceBreach> {
        let (clock, control) = SuspendAwareClock::controlled();
        let early = clock.now();
        control.advance(Duration::from_secs(1));
        let tenure = fence(&clock)?;
        assert!(matches!(
            tenure.renew(early, clock.now()),
            Err(FenceBreach::Backward { .. })
        ));
        Ok(())
    }

    #[test]
    fn an_acknowledgement_slower_than_the_bound_is_not_ownership() {
        let (clock, control) = SuspendAwareClock::controlled();
        let sent = clock.now();
        control.advance(BOUND);
        assert!(matches!(
            SelfFence::confirm(sent, clock.now(), SelfFenceBound::from(BOUND)),
            Err(FenceBreach::Expired { .. })
        ));
    }
}
