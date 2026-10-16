use std::collections::HashMap;
use std::hash::Hash;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEFAULT_REFILL_INTERVAL: Duration = Duration::from_secs(1);
pub const DEFAULT_BURST: u32 = 5;
pub const DEFAULT_IDENTITY_CAPACITY: usize = 10_000;
pub const DEFAULT_IDENTITY_IDLE: Duration = Duration::from_secs(15 * 60);
const SWEEP_THRESHOLD: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RateBurst(NonZeroU32);

impl RateBurst {
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

impl TryFrom<u32> for RateBurst {
    type Error = RateLimitError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        NonZeroU32::new(value).map(Self).ok_or(RateLimitError)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    refill_interval: Duration,
    burst: RateBurst,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rate limit needs a non-zero refill interval and a burst of at least 1")]
pub struct RateLimitError;

impl RateLimit {
    pub fn new(refill_interval: Duration, burst: RateBurst) -> Result<Self, RateLimitError> {
        if refill_interval.is_zero() {
            return Err(RateLimitError);
        }
        Ok(Self { refill_interval, burst })
    }

    pub fn refill_interval(self) -> Duration {
        self.refill_interval
    }

    pub fn burst(self) -> RateBurst {
        self.burst
    }

    fn tolerance(self) -> Duration {
        self.refill_interval * (self.burst.get() - 1)
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            refill_interval: DEFAULT_REFILL_INTERVAL,
            burst: RateBurst(NonZeroU32::MIN.saturating_add(DEFAULT_BURST - 1)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Admitted,
    Limited,
    Saturated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityBound {
    capacity: NonZeroUsize,
    idle: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("identity bound needs a capacity of at least 1 and a non-zero idle expiry")]
pub struct IdentityBoundError;

impl IdentityBound {
    pub fn new(capacity: NonZeroUsize, idle: Duration) -> Result<Self, IdentityBoundError> {
        if idle.is_zero() {
            return Err(IdentityBoundError);
        }
        Ok(Self { capacity, idle })
    }

    pub fn capacity(self) -> NonZeroUsize {
        self.capacity
    }

    pub fn idle(self) -> Duration {
        self.idle
    }
}

impl Default for IdentityBound {
    fn default() -> Self {
        Self {
            capacity: NonZeroUsize::MIN.saturating_add(DEFAULT_IDENTITY_CAPACITY - 1),
            idle: DEFAULT_IDENTITY_IDLE,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Arrival {
    tat: Instant,
    seen: Instant,
}

#[derive(Debug)]
pub struct RateLimiter<K> {
    limit: RateLimit,
    bound: Option<IdentityBound>,
    arrivals: Mutex<HashMap<K, Arrival>>,
}

impl<K: Hash + Eq + Clone> RateLimiter<K> {
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            bound: None,
            arrivals: Mutex::new(HashMap::new()),
        }
    }

    pub fn bounded(limit: RateLimit, bound: IdentityBound) -> Self {
        Self {
            limit,
            bound: Some(bound),
            arrivals: Mutex::new(HashMap::new()),
        }
    }

    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    pub fn bound(&self) -> Option<IdentityBound> {
        self.bound
    }

    pub fn check(&self, key: &K, now: Instant) -> Admission {
        let mut arrivals = match self.arrivals.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let known = arrivals.get(key).copied();
        match self.bound {
            Some(bound) if known.is_none() && arrivals.len() >= bound.capacity.get() => {
                arrivals.retain(|_, arrival| now.saturating_duration_since(arrival.seen) < bound.idle);
                if arrivals.len() >= bound.capacity.get() {
                    return Admission::Saturated;
                }
            }
            Some(_) => {}
            None if arrivals.len() >= SWEEP_THRESHOLD => arrivals.retain(|_, arrival| arrival.tat > now),
            None => {}
        }
        let tat = known.map_or(now, |arrival| arrival.tat).max(now);
        if tat.saturating_duration_since(now) > self.limit.tolerance() {
            arrivals.insert(key.clone(), Arrival { tat, seen: now });
            return Admission::Limited;
        }
        arrivals.insert(
            key.clone(),
            Arrival {
                tat: tat + self.limit.refill_interval,
                seen: now,
            },
        );
        Admission::Admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst_then_one_per_interval() {
        let limiter = RateLimiter::new(RateLimit::default());
        let start = Instant::now();
        for _ in 0..5 {
            assert_eq!(limiter.check(&"ana", start), Admission::Admitted);
        }
        assert_eq!(limiter.check(&"ana", start), Admission::Limited);
        assert_eq!(limiter.check(&"bob", start), Admission::Admitted);
        assert_eq!(
            limiter.check(&"ana", start + Duration::from_millis(999)),
            Admission::Limited
        );
        assert_eq!(
            limiter.check(&"ana", start + Duration::from_secs(1)),
            Admission::Admitted
        );
    }

    #[test]
    fn bounded_state_saturates_until_identities_go_idle() -> Result<(), Box<dyn std::error::Error>> {
        let bound = IdentityBound::new(NonZeroUsize::MIN.saturating_add(1), Duration::from_secs(60))?;
        let limiter = RateLimiter::bounded(RateLimit::default(), bound);
        let start = Instant::now();
        assert_eq!(limiter.check(&"ana", start), Admission::Admitted);
        assert_eq!(limiter.check(&"bob", start), Admission::Admitted);
        assert_eq!(limiter.check(&"cy", start), Admission::Saturated);
        assert_eq!(limiter.check(&"ana", start), Admission::Admitted);
        let later = start + Duration::from_secs(61);
        assert_eq!(limiter.check(&"cy", later), Admission::Admitted);
        assert!(IdentityBound::new(NonZeroUsize::MIN, Duration::ZERO).is_err());
        Ok(())
    }

    #[test]
    fn rejects_degenerate_limits() -> Result<(), RateLimitError> {
        assert!(RateBurst::try_from(0).is_err());
        assert!(RateLimit::new(Duration::ZERO, RateBurst::try_from(5)?).is_err());
        Ok(())
    }
}
