//! Per-actor request limits (decision 0005).
//!
//! Every authenticated API request, HTTP or MCP, counts against one budget
//! per actor, so an agent cannot avoid the limit by switching interfaces. The
//! rate follows the generic cell rate algorithm: a burst of `burst` requests,
//! then `per_minute` requests per minute, evenly spaced. Separately, at most `concurrent`
//! requests of one actor run at a time. A refused request is answered before
//! its body is read and never reaches the application, so it consumes no
//! idempotency key.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use artifree_domain::identity::ActorId;

/// Request budget of one actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLimits {
    /// Requests accepted at once after an idle period.
    pub burst: u32,
    /// Sustained requests per minute.
    pub per_minute: u32,
    /// Requests that may run at the same time.
    pub concurrent: u32,
}

impl Default for RequestLimits {
    /// Generous for one person's web workspace and agents, but bounded for
    /// an agent stuck in a loop: 120 at once, then 1200 per minute (20 per
    /// second), 16 at a time.
    fn default() -> Self {
        Self {
            burst: 120,
            per_minute: 1200,
            concurrent: 16,
        }
    }
}

impl RequestLimits {
    /// Checks that every limit admits at least one request.
    ///
    /// # Errors
    ///
    /// When a limit is zero.
    pub fn validate(self) -> Result<Self, String> {
        if self.burst == 0 || self.per_minute == 0 || self.concurrent == 0 {
            return Err("request limits must be at least 1".into());
        }
        Ok(self)
    }

    fn interval(self) -> Duration {
        Duration::from_secs(60) / self.per_minute
    }

    /// How far ahead of now the schedule may run: room for `burst - 1`
    /// requests beyond the current one.
    fn tolerance(self) -> Duration {
        self.interval()
            .checked_mul(self.burst.saturating_sub(1))
            .unwrap_or(Duration::MAX)
    }
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The rate was exceeded; the next request is admitted after this wait.
    Rate(Duration),
    /// Too many of the actor's requests are running.
    Busy,
    /// The limiter's state is unusable, so it fails closed.
    Unavailable,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rate(_) => formatter.write_str("too many requests; retry after the given delay"),
            Self::Busy => formatter.write_str("too many requests are running; retry shortly"),
            Self::Unavailable => formatter.write_str("request limits are unavailable"),
        }
    }
}

#[derive(Debug, Default)]
struct Budget {
    /// Theoretical arrival time of the next request at the sustained rate.
    next: Option<Instant>,
    in_flight: u32,
}

/// Admits or refuses requests per actor. The map holds one small entry per
/// actor that ever sent a request; today that is the single local owner.
#[derive(Debug)]
pub(crate) struct Limiter {
    limits: RequestLimits,
    budgets: Arc<Mutex<HashMap<ActorId, Budget>>>,
}

impl Limiter {
    pub(crate) fn new(limits: RequestLimits) -> Self {
        Self {
            limits,
            budgets: Arc::default(),
        }
    }

    /// Admits one request of `actor` now, or says why not.
    pub(crate) fn admit(&self, actor: &ActorId) -> Result<Permit, Refusal> {
        self.admit_at(actor, Instant::now())
    }

    fn admit_at(&self, actor: &ActorId, now: Instant) -> Result<Permit, Refusal> {
        let mut budgets = self.budgets.lock().map_err(|_| Refusal::Unavailable)?;
        let budget = budgets.entry(actor.clone()).or_default();
        if budget.in_flight >= self.limits.concurrent {
            return Err(Refusal::Busy);
        }
        let next = budget.next.map_or(now, |next| next.max(now));
        let allowed_from = next
            .checked_sub(self.limits.tolerance())
            .unwrap_or(now.min(next));
        if now < allowed_from {
            return Err(Refusal::Rate(allowed_from - now));
        }
        budget.next = Some(
            next.checked_add(self.limits.interval())
                .ok_or(Refusal::Unavailable)?,
        );
        budget.in_flight += 1;
        Ok(Permit {
            budgets: Arc::clone(&self.budgets),
            actor: actor.clone(),
        })
    }
}

/// One running request. Dropping it frees the actor's concurrency slot.
#[derive(Debug)]
pub(crate) struct Permit {
    budgets: Arc<Mutex<HashMap<ActorId, Budget>>>,
    actor: ActorId,
}

impl Drop for Permit {
    fn drop(&mut self) {
        // A poisoned lock fails every later admission closed, so the count
        // left behind does not matter.
        if let Ok(mut budgets) = self.budgets.lock()
            && let Some(budget) = budgets.get_mut(&self.actor)
        {
            budget.in_flight = budget.in_flight.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(name: &str) -> ActorId {
        name.parse()
            .unwrap_or_else(|error| panic!("invalid fixture actor: {error}"))
    }

    fn limiter(burst: u32, per_minute: u32, concurrent: u32) -> Limiter {
        Limiter::new(RequestLimits {
            burst,
            per_minute,
            concurrent,
        })
    }

    #[test]
    fn a_burst_is_admitted_then_the_rate_applies_until_time_passes() {
        let limiter = limiter(3, 600, 100);
        let agent = actor("agent");
        let start = Instant::now();
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(limiter.admit_at(&agent, start).map_err(|r| r.to_string()));
        }
        assert!(held.iter().all(Result::is_ok), "the burst must be admitted");
        let Err(Refusal::Rate(wait)) = limiter.admit_at(&agent, start) else {
            panic!("the fourth request at once must be refused");
        };
        assert_eq!(wait, Duration::from_millis(100));
        assert!(
            limiter
                .admit_at(&agent, start + Duration::from_millis(99))
                .is_err()
        );
        assert!(
            limiter
                .admit_at(&agent, start + Duration::from_millis(100))
                .is_ok(),
            "one interval later one request is admitted again"
        );
        // After an idle period the full burst is available again.
        let later = start + Duration::from_secs(10);
        for _ in 0..3 {
            assert!(limiter.admit_at(&agent, later).is_ok());
        }
        assert!(limiter.admit_at(&agent, later).is_err());
    }

    #[test]
    fn running_requests_are_capped_and_released_on_drop() {
        let limiter = limiter(100, 6000, 2);
        let agent = actor("agent");
        let now = Instant::now();
        let first = limiter.admit_at(&agent, now);
        let second = limiter.admit_at(&agent, now);
        assert!(first.is_ok() && second.is_ok());
        assert_eq!(limiter.admit_at(&agent, now).err(), Some(Refusal::Busy));
        drop(first);
        assert!(limiter.admit_at(&agent, now).is_ok());
    }

    #[test]
    fn actors_have_separate_budgets() {
        let limiter = limiter(1, 1, 1);
        let now = Instant::now();
        let held = limiter.admit_at(&actor("one"), now);
        assert!(held.is_ok());
        assert!(limiter.admit_at(&actor("one"), now).is_err());
        assert!(limiter.admit_at(&actor("two"), now).is_ok());
    }

    #[test]
    fn zero_limits_are_rejected() {
        for limits in [
            RequestLimits {
                burst: 0,
                ..RequestLimits::default()
            },
            RequestLimits {
                per_minute: 0,
                ..RequestLimits::default()
            },
            RequestLimits {
                concurrent: 0,
                ..RequestLimits::default()
            },
        ] {
            assert!(limits.validate().is_err(), "{limits:?}");
        }
        assert!(RequestLimits::default().validate().is_ok());
    }
}
