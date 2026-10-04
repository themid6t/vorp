//! Per-user traffic quota: a concurrency limit that queues instead of
//! refusing, and a bandwidth budget that slows transfers instead of dropping
//! them. Admins (and development-token sessions) carry no limits.

use std::{
    pin::pin,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};
use vorp_store::UserLimits;

/// How long a request over its user's concurrency limit waits for a slot
/// before it is refused with `503`. The response timeout starts only after
/// the request is dispatched, so waiting here does not shorten it.
const REQUEST_QUEUE_WAIT: Duration = Duration::from_secs(10);

/// Waiting requests allowed per concurrency slot. Each waiter holds an open
/// client connection and its parsed head, so the queue must stay bounded.
const QUEUE_PER_SLOT: usize = 4;

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// One user's shared budget across all of their sessions and tunnels.
pub(crate) struct UserBudget {
    state: Mutex<BudgetState>,
    slot_freed: Notify,
}

struct BudgetState {
    limits: Option<UserLimits>,
    active_requests: usize,
    queued_requests: usize,
    /// Bandwidth bytes available; negative while transfers pay off bytes
    /// already sent, which is what makes them wait.
    tokens: i128,
    refilled: Instant,
}

enum Take {
    Granted,
    Wait,
    QueueFull,
}

impl UserBudget {
    pub(crate) fn new(limits: Option<UserLimits>) -> Self {
        Self {
            state: Mutex::new(BudgetState {
                limits,
                active_requests: 0,
                queued_requests: 0,
                tokens: limits.map_or(0, |l| i128::from(l.bandwidth_bytes_per_sec)),
                refilled: Instant::now(),
            }),
            slot_freed: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies new limits to live traffic. Waiters re-check at once, so a
    /// raised limit admits them without waiting for a release.
    pub(crate) fn set_limits(&self, limits: Option<UserLimits>) {
        self.lock().limits = limits;
        self.slot_freed.notify_waiters();
    }

    /// `None` means unlimited.
    pub(crate) fn max_tunnels(&self) -> Option<usize> {
        self.lock().limits.map(|l| l.max_tunnels as usize)
    }

    /// Waits up to `REQUEST_QUEUE_WAIT` for a request slot. `None` means the
    /// queue was full or the wait expired; the caller answers `503`.
    pub(crate) async fn acquire_request(self: &Arc<Self>) -> Option<RequestSlot> {
        tokio::time::timeout(REQUEST_QUEUE_WAIT, self.wait_for_slot())
            .await
            .ok()
            .flatten()
    }

    // ponytail: a woken waiter re-queues at the back, so a request can lose a
    // freed slot to a newer one; REQUEST_QUEUE_WAIT bounds the unfairness. Use a
    // FIFO ticket queue if users report starvation.
    async fn wait_for_slot(self: &Arc<Self>) -> Option<RequestSlot> {
        let mut queued: Option<QueuedRequest<'_>> = None;
        loop {
            // Registered before checking, so a release between the check and
            // the await still wakes this waiter.
            let mut notified = pin!(self.slot_freed.notified());
            notified.as_mut().enable();
            match self.try_take(queued.is_some()) {
                Take::Granted => return Some(RequestSlot(Arc::clone(self))),
                Take::QueueFull => return None,
                Take::Wait => {
                    if queued.is_none() {
                        queued = Some(QueuedRequest(self));
                    }
                }
            }
            notified.await;
        }
    }

    fn try_take(&self, already_queued: bool) -> Take {
        let mut state = self.lock();
        let Some(limits) = state.limits else {
            state.active_requests += 1;
            return Take::Granted;
        };
        let limit = limits.max_concurrent_requests as usize;
        if state.active_requests < limit {
            state.active_requests += 1;
            return Take::Granted;
        }
        if already_queued {
            return Take::Wait;
        }
        if state.queued_requests >= limit.saturating_mul(QUEUE_PER_SLOT) {
            return Take::QueueFull;
        }
        state.queued_requests += 1;
        Take::Wait
    }

    /// Charges `bytes` to the bandwidth budget and sleeps until they are paid
    /// for. Called before forwarding each chunk, so an over-budget user's
    /// transfer pauses and the pause propagates back through the yamux window
    /// and TCP to the sender; nothing is buffered or dropped.
    pub(crate) async fn throttle(&self, bytes: usize) {
        let wait = self.reserve_bandwidth(bytes, Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn reserve_bandwidth(&self, bytes: usize, now: Instant) -> Duration {
        let mut state = self.lock();
        let Some(limits) = state.limits else {
            return Duration::ZERO;
        };
        // Validated to be at least 1 by the store.
        let rate = i128::from(limits.bandwidth_bytes_per_sec.max(1));
        let elapsed = now.saturating_duration_since(state.refilled).as_nanos();
        let refill = i128::try_from(elapsed)
            .unwrap_or(i128::MAX)
            .saturating_mul(rate)
            / NANOS_PER_SECOND;
        // One second of burst.
        state.tokens = state.tokens.saturating_add(refill).min(rate);
        state.refilled = now;
        state.tokens = state.tokens.saturating_sub(bytes as i128);
        if state.tokens >= 0 {
            return Duration::ZERO;
        }
        let nanos = state
            .tokens
            .saturating_neg()
            .saturating_mul(NANOS_PER_SECOND)
            / rate;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// A user request slot, released on drop.
pub(crate) struct RequestSlot(Arc<UserBudget>);

impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.0.lock().active_requests -= 1;
        self.0.slot_freed.notify_one();
    }
}

/// Counts a waiting request against the queue bound until it leaves the
/// queue, whether granted, refused, or cancelled by the wait timeout.
struct QueuedRequest<'a>(&'a UserBudget);

impl Drop for QueuedRequest<'_> {
    fn drop(&mut self) {
        self.0.lock().queued_requests -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(requests: u32, bandwidth: u64) -> Option<UserLimits> {
        Some(UserLimits {
            max_tunnels: 3,
            bandwidth_bytes_per_sec: bandwidth,
            max_concurrent_requests: requests,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn over_limit_request_waits_for_a_released_slot() {
        let budget = Arc::new(UserBudget::new(limits(1, 1_000)));
        let first = budget.acquire_request().await.expect("first slot");
        let waiter = tokio::spawn({
            let budget = Arc::clone(&budget);
            async move { budget.acquire_request().await.is_some() }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiter.is_finished(), "second request must queue, not fail");
        drop(first);
        assert!(waiter.await.expect("waiter task"));
        assert_eq!(budget.lock().queued_requests, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn queued_request_is_refused_after_the_wait_bound() {
        let budget = Arc::new(UserBudget::new(limits(1, 1_000)));
        let _held = budget.acquire_request().await.expect("slot");
        let started = Instant::now();
        assert!(budget.acquire_request().await.is_none());
        assert_eq!(started.elapsed(), REQUEST_QUEUE_WAIT);
        assert_eq!(budget.lock().queued_requests, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn full_queue_refuses_immediately() {
        let budget = Arc::new(UserBudget::new(limits(1, 1_000)));
        let _held = budget.acquire_request().await.expect("slot");
        let waiters: Vec<_> = (0..QUEUE_PER_SLOT)
            .map(|_| {
                let budget = Arc::clone(&budget);
                tokio::spawn(async move { budget.acquire_request().await.is_some() })
            })
            .collect();
        tokio::task::yield_now().await;
        assert_eq!(budget.lock().queued_requests, QUEUE_PER_SLOT);
        let started = Instant::now();
        assert!(budget.acquire_request().await.is_none());
        assert_eq!(started.elapsed(), Duration::ZERO);
        for waiter in waiters {
            assert!(!waiter.await.expect("waiter task"));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn raising_the_limit_admits_waiters() {
        let budget = Arc::new(UserBudget::new(limits(1, 1_000)));
        let _held = budget.acquire_request().await.expect("slot");
        let waiter = tokio::spawn({
            let budget = Arc::clone(&budget);
            async move { budget.acquire_request().await.is_some() }
        });
        tokio::task::yield_now().await;
        budget.set_limits(limits(2, 1_000));
        assert!(waiter.await.expect("waiter task"));
    }

    #[tokio::test(start_paused = true)]
    async fn exempt_user_is_never_queued_or_throttled() {
        let budget = Arc::new(UserBudget::new(None));
        let slots: Vec<_> =
            futures::future::join_all((0..1_000).map(|_| budget.acquire_request())).await;
        assert!(slots.iter().all(Option::is_some));
        assert_eq!(budget.max_tunnels(), None);
        let started = Instant::now();
        budget.throttle(usize::MAX / 2).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[test]
    fn bandwidth_allows_one_second_burst_then_paces() {
        let budget = UserBudget::new(limits(1, 1_000));
        let start = budget.lock().refilled;
        assert_eq!(budget.reserve_bandwidth(1_000, start), Duration::ZERO);
        assert_eq!(
            budget.reserve_bandwidth(500, start),
            Duration::from_millis(500)
        );
        // Debt accumulates: the next chunk waits behind the previous one.
        assert_eq!(budget.reserve_bandwidth(500, start), Duration::from_secs(1));
        // After the debt is paid off, refill never exceeds the burst.
        let later = start + Duration::from_secs(60);
        assert_eq!(budget.reserve_bandwidth(1_000, later), Duration::ZERO);
        assert_eq!(budget.reserve_bandwidth(1, later), Duration::from_millis(1));
    }

    #[test]
    fn lowering_bandwidth_applies_to_the_next_chunk() {
        let budget = UserBudget::new(limits(1, 1_000_000));
        let start = budget.lock().refilled;
        budget.set_limits(limits(1, 1_000));
        // Stored burst is clamped to the new rate before charging.
        assert_eq!(
            budget.reserve_bandwidth(2_000, start),
            Duration::from_secs(1)
        );
    }
}
