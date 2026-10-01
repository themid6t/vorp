use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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
}
