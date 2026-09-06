//! In-memory per-IP token bucket used to throttle anonymous monitor creation.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    /// `per_hour` requests allowed per address, refilled continuously.
    pub fn new(per_hour: u32) -> Self {
        let capacity = f64::from(per_hour.max(1));
        Self {
            capacity,
            refill_per_sec: capacity / 3600.0,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `ip`. On refusal returns the number of seconds until a token is
    /// available.
    pub fn try_acquire(&self, ip: IpAddr) -> Result<(), u64> {
        self.try_acquire_at(ip, Instant::now())
    }

    pub fn try_acquire_at(&self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > 50_000 {
            // Pathological number of distinct clients: drop full buckets to bound memory.
            let capacity = self.capacity;
            let refill = self.refill_per_sec;
            buckets.retain(|_, b| {
                let refilled = b.tokens + now.duration_since(b.last_refill).as_secs_f64() * refill;
                refilled < capacity
            });
        }
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) / self.refill_per_sec;
            Err(wait.ceil().max(1.0) as u64)
        }
    }

    /// Remove buckets that are full again (nothing to remember about them).
    pub fn prune(&self) {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let capacity = self.capacity;
        let refill = self.refill_per_sec;
        buckets.retain(|_, b| {
            let refilled = b.tokens + now.duration_since(b.last_refill).as_secs_f64() * refill;
            refilled < capacity
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn allows_burst_then_blocks_then_refills() {
        let limiter = RateLimiter::new(10);
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let start = Instant::now();
        for _ in 0..10 {
            assert!(limiter.try_acquire_at(ip, start).is_ok());
        }
        let wait = limiter.try_acquire_at(ip, start).unwrap_err();
        assert!(
            (300..=360).contains(&wait),
            "expected ~6 minutes, got {wait}s"
        );

        // Another address is independent.
        let other: IpAddr = "203.0.113.8".parse().unwrap();
        assert!(limiter.try_acquire_at(other, start).is_ok());

        // After six minutes one token is back.
        let later = start + Duration::from_secs(6 * 60 + 1);
        assert!(limiter.try_acquire_at(ip, later).is_ok());
        assert!(limiter.try_acquire_at(ip, later).is_err());

        // A full hour restores the whole burst.
        let much_later = start + Duration::from_secs(3600 * 2);
        for _ in 0..10 {
            assert!(limiter.try_acquire_at(ip, much_later).is_ok());
        }
        limiter.prune();
    }
}
