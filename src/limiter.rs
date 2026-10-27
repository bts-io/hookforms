//! In-memory replacements for the Redis keys the Python version used.
//!
//! - `SlidingWindow`: the per-IP request limiter (Redis sorted set
//!   `ratelimit:{ip}`, 100 requests per 60 s), used by `middleware.rs`.
//! - `Counters`: `INCR` plus `EXPIRE` on first hit, for `auth_fail:{ip}`
//!   (auth lockout) and the per-inbox email rate keys in hooks and channels.
//!
//! State lives in the process, so it resets on restart. That is fine for a
//! single-instance service and matches the decision to drop Redis.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct SlidingWindow {
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindow {
    /// Record a hit and return how many hits were already in the window before it
    /// (the Python code reads `ZCARD` before `ZADD`).
    pub fn hit(&self, key: &str, window: Duration) -> usize {
        let now = Instant::now();
        let mut map = self.hits.lock().expect("limiter lock");
        if map.len() > 10_000 {
            map.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < window));
        }
        let q = map.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        let before = q.len();
        q.push_back(now);
        before
    }
}

#[derive(Debug, Default)]
pub struct Counters {
    map: Mutex<HashMap<String, (u64, Instant)>>,
}

impl Counters {
    /// `INCR key`, setting a `ttl` on the first increment. Returns the new value.
    pub fn incr(&self, key: &str, ttl: Duration) -> u64 {
        let now = Instant::now();
        let mut map = self.map.lock().expect("counter lock");
        if map.len() > 10_000 {
            map.retain(|_, (_, exp)| *exp > now);
        }
        let entry = map.entry(key.to_string()).or_insert((0, now + ttl));
        if entry.1 <= now {
            *entry = (0, now + ttl);
        }
        entry.0 += 1;
        entry.0
    }

    /// `GET key`, 0 when missing or expired.
    pub fn get(&self, key: &str) -> u64 {
        let map = self.map.lock().expect("counter lock");
        match map.get(key) {
            Some((n, exp)) if *exp > Instant::now() => *n,
            _ => 0,
        }
    }

    /// `DEL key`.
    pub fn remove(&self, key: &str) {
        self.map.lock().expect("counter lock").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_expire() {
        let c = Counters::default();
        assert_eq!(c.incr("a", Duration::from_secs(60)), 1);
        assert_eq!(c.incr("a", Duration::from_secs(60)), 2);
        assert_eq!(c.get("a"), 2);
        c.remove("a");
        assert_eq!(c.get("a"), 0);
        assert_eq!(c.incr("b", Duration::ZERO), 1);
        assert_eq!(c.get("b"), 0);
    }

    #[test]
    fn window_counts_previous_hits() {
        let w = SlidingWindow::default();
        assert_eq!(w.hit("ip", Duration::from_secs(60)), 0);
        assert_eq!(w.hit("ip", Duration::from_secs(60)), 1);
        assert_eq!(w.hit("other", Duration::from_secs(60)), 0);
    }
}
