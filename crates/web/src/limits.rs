use std::sync::Arc;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

const WINDOW: Duration = Duration::from_secs(60);
const GLOBAL_ATTEMPTS: u32 = 120;
const ACCOUNT_ATTEMPTS: u32 = 10;
const MAX_ACCOUNTS: usize = 4096;
const ARGON2_CONCURRENCY: usize = 4;
pub(crate) const MAX_TRAFFIC_STREAMS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LimitError {
    Busy,
    Poisoned,
}

struct WindowState {
    started: Instant,
    global_attempts: u32,
    accounts: HashMap<String, u32>,
}

pub(crate) struct AuthLimiter {
    state: Mutex<WindowState>,
    argon2_slots: Arc<Semaphore>,
}

impl AuthLimiter {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(WindowState {
                started: Instant::now(),
                global_attempts: 0,
                accounts: HashMap::new(),
            }),
            argon2_slots: Arc::new(Semaphore::new(ARGON2_CONCURRENCY)),
        }
    }

    pub(crate) fn admit(&self, account: &str) -> Result<OwnedSemaphorePermit, LimitError> {
        self.check_at(account, Instant::now())?;
        self.argon2_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| LimitError::Busy)
    }

    fn check_at(&self, account: &str, now: Instant) -> Result<(), LimitError> {
        if account.len() > 320 {
            return Err(LimitError::Busy);
        }
        let mut state = self.state.lock().map_err(|_| LimitError::Poisoned)?;
        if now.saturating_duration_since(state.started) >= WINDOW {
            state.started = now;
            state.global_attempts = 0;
            state.accounts.clear();
        }
        let key = account.trim().to_ascii_lowercase();
        if state.global_attempts >= GLOBAL_ATTEMPTS
            || state
                .accounts
                .get(&key)
                .is_some_and(|attempts| *attempts >= ACCOUNT_ATTEMPTS)
            || (!state.accounts.contains_key(&key) && state.accounts.len() >= MAX_ACCOUNTS)
        {
            return Err(LimitError::Busy);
        }
        state.global_attempts += 1;
        *state.accounts.entry(key).or_default() += 1;
        Ok(())
    }
}

pub(crate) fn traffic_stream_permit(
    slots: &Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, LimitError> {
    slots.clone().try_acquire_owned().map_err(|e| match e {
        TryAcquireError::NoPermits | TryAcquireError::Closed => LimitError::Busy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_and_global_windows_are_bounded() {
        let limiter = AuthLimiter::new();
        let start = Instant::now();
        for _ in 0..ACCOUNT_ATTEMPTS {
            assert_eq!(limiter.check_at("Alice@Example.test", start), Ok(()));
        }
        assert_eq!(
            limiter.check_at(" alice@example.test ", start),
            Err(LimitError::Busy)
        );
        for index in 0..GLOBAL_ATTEMPTS - ACCOUNT_ATTEMPTS {
            assert_eq!(limiter.check_at(&format!("user{index}"), start), Ok(()));
        }
        assert_eq!(limiter.check_at("extra", start), Err(LimitError::Busy));
        assert_eq!(
            limiter.check_at("alice@example.test", start + WINDOW),
            Ok(())
        );
    }

    #[test]
    fn concurrent_argon2_work_and_streams_release_capacity() {
        let limiter = AuthLimiter::new();
        let permits = (0..ARGON2_CONCURRENCY)
            .map(|i| limiter.admit(&format!("user{i}")))
            .collect::<Result<Vec<_>, _>>()
            .expect("permits");
        assert!(matches!(limiter.admit("overflow"), Err(LimitError::Busy)));
        drop(permits);
        assert!(limiter.admit("after").is_ok());
        let slots = Arc::new(Semaphore::new(MAX_TRAFFIC_STREAMS));
        let permits = (0..MAX_TRAFFIC_STREAMS)
            .map(|_| traffic_stream_permit(&slots))
            .collect::<Result<Vec<_>, _>>()
            .expect("streams");
        assert!(matches!(
            traffic_stream_permit(&slots),
            Err(LimitError::Busy)
        ));
        drop(permits);
        assert!(traffic_stream_permit(&slots).is_ok());
    }
}
