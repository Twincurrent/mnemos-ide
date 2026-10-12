// SPDX-License-Identifier: AGPL-3.0-only
//! Caps on spawning, so a looping or manipulated coordinator cannot fill the
//! machine with sessions.

use std::collections::VecDeque;
use std::time::Duration;

use instant::Instant;

/// Most spawned sessions alive at once, across all coordinators.
pub const MAX_LIVE: usize = 6;
/// Most spawns in any [`WINDOW`].
pub const MAX_PER_WINDOW: usize = 3;
pub const WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum LimitError {
    TooManyLive { live: usize },
    TooFast { retry_after: Duration },
}

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitError::TooManyLive { live } => write!(
                f,
                "{live} spawned sessions are alive (the cap is {MAX_LIVE}); retire one first"
            ),
            LimitError::TooFast { retry_after } => write!(
                f,
                "at most {MAX_PER_WINDOW} spawns per minute; retry in {} s",
                retry_after.as_secs().max(1)
            ),
        }
    }
}

/// Remembers recent spawn times. The live count comes from the caller, which
/// knows which spawned panes still exist.
#[derive(Debug, Default)]
pub struct SpawnLimiter {
    recent: VecDeque<Instant>,
}

impl SpawnLimiter {
    /// Checks the caps for a spawn at `now` and, when allowed, records it.
    pub fn try_spawn(&mut self, live: usize, now: Instant) -> Result<(), LimitError> {
        if live >= MAX_LIVE {
            return Err(LimitError::TooManyLive { live });
        }
        while let Some(first) = self.recent.front() {
            if now.duration_since(*first) >= WINDOW {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.recent.len() >= MAX_PER_WINDOW {
            let oldest = *self.recent.front().expect("non-empty");
            return Err(LimitError::TooFast {
                retry_after: WINDOW - now.duration_since(oldest),
            });
        }
        self.recent.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_live_sessions() {
        let mut limiter = SpawnLimiter::default();
        assert_eq!(
            limiter.try_spawn(MAX_LIVE, Instant::now()),
            Err(LimitError::TooManyLive { live: MAX_LIVE })
        );
    }

    #[test]
    fn caps_rate_and_recovers() {
        let mut limiter = SpawnLimiter::default();
        let t0 = Instant::now();
        for i in 0..MAX_PER_WINDOW {
            assert_eq!(limiter.try_spawn(i, t0 + Duration::from_secs(i as u64)), Ok(()));
        }
        let t = t0 + Duration::from_secs(10);
        match limiter.try_spawn(0, t) {
            Err(LimitError::TooFast { retry_after }) => {
                assert_eq!(retry_after, Duration::from_secs(50))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(limiter.try_spawn(0, t0 + WINDOW), Ok(()));
    }

    #[test]
    fn refused_spawns_are_not_counted() {
        let mut limiter = SpawnLimiter::default();
        let t0 = Instant::now();
        for _ in 0..10 {
            let _ = limiter.try_spawn(MAX_LIVE, t0);
        }
        assert_eq!(limiter.try_spawn(0, t0), Ok(()));
    }
}
