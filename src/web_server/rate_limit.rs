use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};

const WINDOW_SECONDS: u64 = 60;
const MAX_LIMITER_ENTRIES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum RateBucket {
    GuestSession,
    RoomRead,
    RoomMutation,
    Admission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRateLimitConfig {
    pub maximum_client_entries: usize,
    pub guest_sessions_per_minute: u32,
    pub room_reads_per_minute: u32,
    pub room_mutations_per_minute: u32,
    pub admissions_per_minute: u32,
}

impl Default for WebRateLimitConfig {
    fn default() -> Self {
        Self {
            maximum_client_entries: 65_536,
            guest_sessions_per_minute: 20,
            room_reads_per_minute: 240,
            room_mutations_per_minute: 60,
            admissions_per_minute: 60,
        }
    }
}

impl WebRateLimitConfig {
    pub fn validate(self) -> Result<(), WebRateLimitConfigError> {
        if self.maximum_client_entries == 0
            || self.maximum_client_entries > MAX_LIMITER_ENTRIES
            || self.guest_sessions_per_minute == 0
            || self.room_reads_per_minute == 0
            || self.room_mutations_per_minute == 0
            || self.admissions_per_minute == 0
        {
            return Err(WebRateLimitConfigError);
        }
        Ok(())
    }

    const fn limit(self, bucket: RateBucket) -> u32 {
        match bucket {
            RateBucket::GuestSession => self.guest_sessions_per_minute,
            RateBucket::RoomRead => self.room_reads_per_minute,
            RateBucket::RoomMutation => self.room_mutations_per_minute,
            RateBucket::Admission => self.admissions_per_minute,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRateLimitConfigError;

impl fmt::Display for WebRateLimitConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid hosted web rate-limit configuration")
    }
}

impl std::error::Error for WebRateLimitConfigError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RateKey {
    address: IpAddr,
    bucket: RateBucket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    started_at: u64,
    requests: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RateLimited {
    pub retry_after_seconds: u64,
}

pub(super) struct WebRateLimiter {
    config: WebRateLimitConfig,
    windows: Mutex<HashMap<RateKey, Window>>,
}

impl WebRateLimiter {
    pub(super) fn new(config: WebRateLimitConfig) -> Result<Self, WebRateLimitConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            windows: Mutex::new(HashMap::with_capacity(
                config.maximum_client_entries.min(1_024),
            )),
        })
    }

    pub(super) fn check(
        &self,
        address: IpAddr,
        bucket: RateBucket,
        now: u64,
    ) -> Result<(), RateLimited> {
        let mut windows = lock_recover(&self.windows);
        let key = RateKey { address, bucket };
        if !windows.contains_key(&key) && windows.len() >= self.config.maximum_client_entries {
            // Cleanup is proportional to the bounded table only when a new
            // client would otherwise exceed capacity. Ordinary requests and
            // repeat clients remain O(1).
            windows.retain(|_, window| now.saturating_sub(window.started_at) < WINDOW_SECONDS);
        }
        if !windows.contains_key(&key) && windows.len() >= self.config.maximum_client_entries {
            return Err(RateLimited {
                retry_after_seconds: WINDOW_SECONDS,
            });
        }
        let window = windows.entry(key).or_insert(Window {
            started_at: now,
            requests: 0,
        });
        if now.saturating_sub(window.started_at) >= WINDOW_SECONDS {
            *window = Window {
                started_at: now,
                requests: 0,
            };
        }
        if window.requests >= self.config.limit(bucket) {
            return Err(RateLimited {
                retry_after_seconds: WINDOW_SECONDS
                    .saturating_sub(now.saturating_sub(window.started_at))
                    .max(1),
            });
        }
        window.requests = window.requests.saturating_add(1);
        Ok(())
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_windows_are_bounded_and_reset_without_unbounded_client_state() {
        let limiter = WebRateLimiter::new(WebRateLimitConfig {
            maximum_client_entries: 1,
            guest_sessions_per_minute: 2,
            ..WebRateLimitConfig::default()
        })
        .unwrap();
        let first = "127.0.0.1".parse().unwrap();
        let second = "127.0.0.2".parse().unwrap();
        assert!(limiter.check(first, RateBucket::GuestSession, 100).is_ok());
        assert!(limiter.check(first, RateBucket::GuestSession, 101).is_ok());
        assert_eq!(
            limiter.check(first, RateBucket::GuestSession, 102),
            Err(RateLimited {
                retry_after_seconds: 58
            })
        );
        assert!(
            limiter
                .check(second, RateBucket::GuestSession, 102)
                .is_err()
        );
        assert!(limiter.check(second, RateBucket::GuestSession, 160).is_ok());
    }
}
