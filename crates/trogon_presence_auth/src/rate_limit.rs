use std::time::Instant;

pub use trogon_presence::{Admission, IdentityBound, IdentityBoundError, RateBurst, RateLimit, RateLimitError};

use crate::claims::Subject;
use crate::ids::TenantId;

#[derive(Debug)]
pub struct RateLimiter {
    inner: trogon_presence::RateLimiter<(TenantId, Subject)>,
}

impl RateLimiter {
    pub fn new(limit: RateLimit, bound: IdentityBound) -> Self {
        Self {
            inner: trogon_presence::RateLimiter::bounded(limit, bound),
        }
    }

    pub fn check(&self, tenant: &TenantId, sub: &Subject, now: Instant) -> Admission {
        self.inner.check(&(tenant.clone(), sub.clone()), now)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use super::*;

    #[test]
    fn allows_burst_then_one_per_interval() -> Result<(), Box<dyn std::error::Error>> {
        let limiter = RateLimiter::new(RateLimit::default(), IdentityBound::default());
        let tenant: TenantId = "t".parse()?;
        let ana = Subject::try_from("ana".to_owned())?;
        let bob = Subject::try_from("bob".to_owned())?;
        let start = Instant::now();
        for _ in 0..5 {
            assert_eq!(limiter.check(&tenant, &ana, start), Admission::Admitted);
        }
        assert_eq!(limiter.check(&tenant, &ana, start), Admission::Limited);
        assert_eq!(limiter.check(&tenant, &bob, start), Admission::Admitted);
        assert_eq!(
            limiter.check(&tenant, &ana, start + Duration::from_millis(999)),
            Admission::Limited
        );
        assert_eq!(
            limiter.check(&tenant, &ana, start + Duration::from_secs(1)),
            Admission::Admitted
        );
        Ok(())
    }

    #[test]
    fn saturates_at_the_identity_cap() -> Result<(), Box<dyn std::error::Error>> {
        let bound = IdentityBound::new(NonZeroUsize::MIN, Duration::from_secs(60))?;
        let limiter = RateLimiter::new(RateLimit::default(), bound);
        let tenant: TenantId = "t".parse()?;
        let start = Instant::now();
        assert_eq!(
            limiter.check(&tenant, &Subject::try_from("ana".to_owned())?, start),
            Admission::Admitted
        );
        assert_eq!(
            limiter.check(&tenant, &Subject::try_from("bob".to_owned())?, start),
            Admission::Saturated
        );
        Ok(())
    }
}
