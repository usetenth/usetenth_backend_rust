//! In-memory fixed-window limiter, per process; use a shared store if the API is scaled out.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    /// True when `key` has already hit `limit` events inside its current window.
    pub fn is_blocked(&self, key: &str, limit: u32, window: Duration) -> bool {
        let windows = self.windows.lock().unwrap();
        windows.get(key).is_some_and(|(start, count)| start.elapsed() < window && *count >= limit)
    }

    pub fn record(&self, key: &str, window: Duration) {
        let mut windows = self.windows.lock().unwrap();
        if windows.len() > 10_000 {
            windows.retain(|_, (start, _)| start.elapsed() < window);
        }
        let entry = windows.entry(key.to_string()).or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= window {
            *entry = (Instant::now(), 0);
        }
        entry.1 += 1;
    }

    pub fn allow(&self, key: &str, limit: u32, window: Duration) -> bool {
        if self.is_blocked(key, limit, window) {
            return false;
        }
        self.record(key, window);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_the_limit_and_recovers() {
        let limiter = RateLimiter::default();
        let window = Duration::from_millis(60);
        assert!(limiter.allow("a", 2, window));
        assert!(limiter.allow("a", 2, window));
        assert!(!limiter.allow("a", 2, window));
        assert!(limiter.allow("b", 2, window), "keys are independent");
        std::thread::sleep(Duration::from_millis(80));
        assert!(limiter.allow("a", 2, window));
    }

    #[test]
    fn recording_failures_does_not_block_until_the_limit() {
        let limiter = RateLimiter::default();
        let window = Duration::from_secs(60);
        limiter.record("k", window);
        assert!(!limiter.is_blocked("k", 2, window));
        limiter.record("k", window);
        assert!(limiter.is_blocked("k", 2, window));
    }
}
