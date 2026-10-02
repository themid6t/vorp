use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const RATE_TOKEN: u64 = 1_000_000;
const MAX_RATE_BUCKETS: usize = 4096;

struct RateBucket {
    tokens: u64,
    updated: Instant,
}

/// Per-peer token bucket allowing `per_second` requests with a burst of twice
/// that. The default (200/s) is sized for one browser loading an asset-heavy
/// page through a tunnel; the old nginx value (20/s) throttled page loads.
pub(crate) struct RequestRateLimiter {
    buckets: Mutex<HashMap<IpAddr, RateBucket>>,
    capacity: u64,
    refill_per_millisecond: u64,
}

impl RequestRateLimiter {
    pub(crate) fn new(per_second: u64) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: per_second.saturating_mul(2).saturating_mul(RATE_TOKEN),
            refill_per_millisecond: per_second.saturating_mul(RATE_TOKEN / 1000),
        }
    }

    pub(crate) fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !buckets.contains_key(&ip) && buckets.len() >= MAX_RATE_BUCKETS {
            buckets.retain(|_, bucket| {
                now.saturating_duration_since(bucket.updated) < Duration::from_secs(60)
            });
            if buckets.len() >= MAX_RATE_BUCKETS {
                return false;
            }
        }
        let bucket = buckets.entry(ip).or_insert(RateBucket {
            tokens: self.capacity,
            updated: now,
        });
        let elapsed_ms = now.saturating_duration_since(bucket.updated).as_millis();
        let refill = elapsed_ms
            .saturating_mul(u128::from(self.refill_per_millisecond))
            .min(u128::from(u64::MAX)) as u64;
        bucket.tokens = bucket.tokens.saturating_add(refill).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens < RATE_TOKEN {
            return false;
        }
        bucket.tokens -= RATE_TOKEN;
        true
    }
}

/// Bounds TLS handshakes and established connections before dispatching work.
pub(crate) struct ConnectionLimiter {
    global: Arc<Semaphore>,
    by_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    per_ip_limit: usize,
}

pub(crate) struct ConnectionPermit {
    ip: IpAddr,
    by_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    _global: OwnedSemaphorePermit,
}

impl ConnectionLimiter {
    pub(crate) fn new(global_limit: usize, per_ip_limit: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            by_ip: Arc::new(Mutex::new(HashMap::new())),
            per_ip_limit,
        }
    }

    pub(crate) fn try_acquire(&self, ip: IpAddr) -> Option<ConnectionPermit> {
        let global = Arc::clone(&self.global).try_acquire_owned().ok()?;
        let mut by_ip = self
            .by_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = by_ip.entry(ip).or_default();
        if *active >= self.per_ip_limit {
            return None;
        }
        *active += 1;
        Some(ConnectionPermit {
            ip,
            by_ip: Arc::clone(&self.by_ip),
            _global: global,
        })
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut by_ip = self
            .by_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(active) = by_ip.get_mut(&self.ip) {
            *active -= 1;
            if *active == 0 {
                by_ip.remove(&self.ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_released_on_drop() {
        let limiter = ConnectionLimiter::new(2, 1);
        let first_ip = IpAddr::from([127, 0, 0, 1]);
        let second_ip = IpAddr::from([127, 0, 0, 2]);
        let third_ip = IpAddr::from([127, 0, 0, 3]);
        let first = limiter.try_acquire(first_ip).expect("first connection");
        assert!(limiter.try_acquire(first_ip).is_none());
        let second = limiter.try_acquire(second_ip).expect("second connection");
        assert!(limiter.try_acquire(third_ip).is_none());
        drop(first);
        assert!(limiter.try_acquire(first_ip).is_some());
        drop(second);
        assert!(limiter.try_acquire(third_ip).is_some());
    }

    #[test]
    fn request_rate_has_burst_and_refills_without_exceeding_capacity() {
        let limiter = RequestRateLimiter::new(200);
        let ip = IpAddr::from([127, 0, 0, 1]);
        let now = Instant::now();
        for _ in 0..400 {
            assert!(limiter.allow_at(ip, now));
        }
        assert!(!limiter.allow_at(ip, now));
        assert!(!limiter.allow_at(ip, now + Duration::from_millis(4)));
        assert!(limiter.allow_at(ip, now + Duration::from_millis(5)));
        assert!(!limiter.allow_at(ip, now + Duration::from_millis(5)));
        assert!(limiter.allow_at(IpAddr::from([127, 0, 0, 2]), now));
    }
}
