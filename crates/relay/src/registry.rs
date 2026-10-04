use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use vorp_protocol::CloseReason;
use yamux::Stream;

use crate::{quota::UserBudget, subdomain::SlugError};

pub(crate) type SessionKey = (i64, String);

const TRAFFIC_EVENTS_PER_USER: usize = 256;

pub(crate) struct Session {
    pub id: String,
    pub key: SessionKey,
    pub token_id: Option<i64>,
    pub user_id: i64,
    /// Shared with every other live session of the same user.
    pub budget: Arc<UserBudget>,
    pub cancel: CancellationToken,
    pub open: mpsc::Sender<oneshot::Sender<Result<Stream, yamux::ConnectionError>>>,
    pub(crate) teardown_started: AtomicBool,
    pub close_reason: Mutex<CloseReason>,
}

pub(crate) struct Tunnel {
    pub session: Arc<Session>,
    pub subdomain: String,
    pub concurrency_limit: usize,
    pub active: std::sync::atomic::AtomicUsize,
    pub websocket_limit: usize,
    pub websocket_active: std::sync::atomic::AtomicUsize,
    pub upstream_hint: Option<String>,
    pub cancel: CancellationToken,
}

/// Per-tunnel concurrency caps, populated from the relay's edge limits so a
/// future per-user policy can set them per tunnel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TunnelLimits {
    pub requests: usize,
    pub websockets: usize,
}

impl From<&crate::EdgeLimits> for TunnelLimits {
    fn from(limits: &crate::EdgeLimits) -> Self {
        Self {
            requests: limits.tunnel_requests,
            websockets: limits.tunnel_websockets,
        }
    }
}

impl Tunnel {
    pub(crate) fn new(
        session: Arc<Session>,
        subdomain: String,
        upstream_hint: Option<String>,
        limits: TunnelLimits,
    ) -> Self {
        Self {
            session,
            subdomain,
            concurrency_limit: limits.requests,
            active: std::sync::atomic::AtomicUsize::new(0),
            websocket_limit: limits.websockets,
            websocket_active: std::sync::atomic::AtomicUsize::new(0),
            upstream_hint,
            cancel: CancellationToken::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TunnelInsertError {
    #[error("subdomain already in use")]
    Taken,
    #[error("user tunnel limit reached")]
    LimitReached,
    #[error(transparent)]
    Slug(#[from] SlugError),
}

#[derive(Default)]
pub(crate) struct Registry {
    sessions: Mutex<HashMap<SessionKey, Arc<Session>>>,
    // ponytail: entries outlive their user's sessions; bounded by the number
    // of accounts, so not worth evicting.
    budgets: Mutex<HashMap<i64, Arc<UserBudget>>>,
    revoked_tokens: Mutex<HashSet<i64>>,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
    /// Recent events per user, so a busy user cannot evict a quiet user's
    /// history from a shared ring.
    traffic: Mutex<HashMap<i64, VecDeque<vorp_web::TrafficEvent>>>,
}

impl Registry {
    pub fn insert_session(&self, session: Arc<Session>) -> Result<Option<Arc<Session>>, ()> {
        // The same lock is held by disconnect_token through its session snapshot.
        // A revocation that wins this race rejects registration; one that loses
        // sees the freshly inserted session and tears it down.
        let revoked = self
            .revoked_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if session.token_id.is_some_and(|id| revoked.contains(&id)) {
            return Err(());
        }
        Ok(self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.key.clone(), session))
    }

    /// The user's budget with `limits` applied, created on first use. Every
    /// session of one user shares it, so quotas span all of their agents.
    pub fn budget(&self, user_id: i64, limits: Option<vorp_store::UserLimits>) -> Arc<UserBudget> {
        let mut budgets = self
            .budgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(budget) = budgets.get(&user_id) {
            budget.set_limits(limits);
            return Arc::clone(budget);
        }
        let budget = Arc::new(UserBudget::new(limits));
        budgets.insert(user_id, Arc::clone(&budget));
        budget
    }

    pub fn budget_user_ids(&self) -> Vec<i64> {
        self.budgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    pub fn insert_named_tunnel(&self, tunnel: Arc<Tunnel>) -> Result<(), TunnelInsertError> {
        let mut map = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tunnel.session.cancel.is_cancelled() || map.contains_key(&tunnel.subdomain) {
            return Err(TunnelInsertError::Taken);
        }
        if at_tunnel_limit(&map, &tunnel.session) {
            return Err(TunnelInsertError::LimitReached);
        }
        map.insert(tunnel.subdomain.clone(), tunnel);
        Ok(())
    }

    pub fn allocate_tunnel(
        &self,
        session: Arc<Session>,
        upstream_hint: Option<String>,
        limits: TunnelLimits,
        mut generate: impl FnMut() -> Result<String, SlugError>,
    ) -> Result<Arc<Tunnel>, TunnelInsertError> {
        let mut map = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if at_tunnel_limit(&map, &session) {
            return Err(TunnelInsertError::LimitReached);
        }
        for _ in 0..8 {
            if session.cancel.is_cancelled() {
                return Err(SlugError::Collisions.into());
            }
            let slug = generate()?;
            if let std::collections::hash_map::Entry::Vacant(slot) = map.entry(slug.clone()) {
                let tunnel = Arc::new(Tunnel::new(session, slug, upstream_hint, limits));
                slot.insert(Arc::clone(&tunnel));
                return Ok(tunnel);
            }
        }
        Err(SlugError::Collisions.into())
    }

    pub fn tunnel(&self, subdomain: &str) -> Option<Arc<Tunnel>> {
        self.tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(subdomain)
            .cloned()
    }

    pub fn tunnels_for_user(&self, user_id: i64) -> Vec<vorp_web::TunnelView> {
        self.tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|t| t.session.user_id == user_id)
            .map(|t| vorp_web::TunnelView {
                subdomain: t.subdomain.clone(),
                machine_id: t.session.key.1.clone(),
                upstream_hint: t.upstream_hint.clone(),
                active_requests: t.active.load(Ordering::Acquire),
            })
            .collect()
    }

    pub fn close_for_user(&self, user_id: i64, subdomain: &str) -> bool {
        let tunnel = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(subdomain)
            .filter(|t| t.session.user_id == user_id)
            .cloned();
        if let Some(tunnel) = tunnel {
            tunnel.cancel.cancel();
            self.remove_tunnel_if_same(&tunnel)
        } else {
            false
        }
    }

    pub fn record_traffic(&self, user_id: i64, event: vorp_web::TrafficEvent) {
        let mut traffic = self
            .traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let events = traffic.entry(user_id).or_default();
        if events.len() == TRAFFIC_EVENTS_PER_USER {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub fn recent_traffic(&self, user_id: i64) -> Vec<vorp_web::TrafficEvent> {
        self.traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&user_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn remove_tunnel_if_same(&self, tunnel: &Arc<Tunnel>) -> bool {
        let mut map = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map
            .get(&tunnel.subdomain)
            .is_some_and(|stored| Arc::ptr_eq(stored, tunnel))
        {
            map.remove(&tunnel.subdomain);
            return true;
        }
        false
    }

    pub fn teardown(&self, session: &Arc<Session>) -> Vec<Arc<Tunnel>> {
        if session.teardown_started.swap(true, Ordering::AcqRel) {
            return Vec::new();
        }
        session.cancel.cancel();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions
            .get(&session.key)
            .is_some_and(|stored| Arc::ptr_eq(stored, session))
        {
            sessions.remove(&session.key);
        }
        drop(sessions);
        let mut tunnels = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owned: Vec<_> = tunnels
            .values()
            .filter(|t| Arc::ptr_eq(&t.session, session))
            .cloned()
            .collect();
        for tunnel in &owned {
            tunnel.cancel.cancel();
            if tunnels
                .get(&tunnel.subdomain)
                .is_some_and(|stored| Arc::ptr_eq(stored, tunnel))
            {
                tunnels.remove(&tunnel.subdomain);
            }
        }
        owned
    }

    pub fn disconnect_token(&self, token_id: i64) -> Vec<Arc<Session>> {
        let mut revoked = self
            .revoked_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        revoked.insert(token_id);
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|s| s.token_id == Some(token_id))
            .cloned()
            .collect();
        drop(revoked);
        for session in &sessions {
            *session
                .close_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = CloseReason::Revoked;
            self.teardown(session);
        }
        sessions
    }
}

/// Checked under the tunnel-map lock, so concurrent registrations cannot
/// both take the last slot.
// ponytail: scans every live tunnel; keep a per-user count if the map grows
// to many thousands.
fn at_tunnel_limit(map: &HashMap<String, Arc<Tunnel>>, session: &Session) -> bool {
    let Some(max) = session.budget.max_tunnels() else {
        return false;
    };
    map.values()
        .filter(|t| t.session.user_id == session.user_id)
        .count()
        >= max
}

impl Tunnel {
    pub fn try_acquire(self: &Arc<Self>) -> Option<TunnelPermit> {
        let previous = self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.concurrency_limit).then_some(current + 1)
            });
        previous.ok().map(|_| TunnelPermit(Arc::clone(self)))
    }

    pub fn try_acquire_websocket(self: &Arc<Self>) -> Option<WebSocketPermit> {
        let previous =
            self.websocket_active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < self.websocket_limit).then_some(current + 1)
                });
        previous.ok().map(|_| WebSocketPermit(Arc::clone(self)))
    }
}

pub(crate) struct TunnelPermit(Arc<Tunnel>);

impl Drop for TunnelPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Release);
    }
}

pub(crate) struct WebSocketPermit(Arc<Tunnel>);

impl Drop for WebSocketPermit {
    fn drop(&mut self) {
        self.0.websocket_active.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: TunnelLimits = TunnelLimits {
        requests: 1,
        websockets: 1,
    };

    fn session(id: &str) -> Arc<Session> {
        user_session(id, 1, Arc::new(UserBudget::new(None)))
    }

    fn user_session(id: &str, user_id: i64, budget: Arc<UserBudget>) -> Arc<Session> {
        let (open, _) = mpsc::channel(1);
        Arc::new(Session {
            id: id.into(),
            key: (user_id, "machine".into()),
            token_id: Some(10),
            user_id,
            budget,
            cancel: CancellationToken::new(),
            open,
            teardown_started: AtomicBool::new(false),
            close_reason: Mutex::new(CloseReason::Forced),
        })
    }

    #[test]
    fn reconnect_displaces_without_deleting_replacement() {
        let registry = Registry::default();
        let old = session("old");
        registry.insert_session(Arc::clone(&old)).unwrap();
        let new = session("new");
        registry.insert_session(Arc::clone(&new)).unwrap();
        registry.teardown(&old);
        assert!(
            registry
                .sessions
                .lock()
                .unwrap()
                .get(&new.key)
                .is_some_and(|s| Arc::ptr_eq(s, &new))
        );
        registry.teardown(&new);
        assert!(registry.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn teardown_once_and_tunnel_replacement() {
        let registry = Registry::default();
        let old_session = session("old");
        let new_session = session("new");
        let make = |s: Arc<Session>| Arc::new(Tunnel::new(s, "app".into(), None, ONE));
        let old = make(Arc::clone(&old_session));
        let new = make(Arc::clone(&new_session));
        registry.insert_named_tunnel(Arc::clone(&old)).unwrap();
        registry
            .tunnels
            .lock()
            .unwrap()
            .insert("app".into(), Arc::clone(&new));
        assert!(!registry.remove_tunnel_if_same(&old));
        registry.teardown(&old_session);
        assert!(
            registry
                .tunnel("app")
                .is_some_and(|t| Arc::ptr_eq(&t, &new))
        );
        assert!(registry.teardown(&old_session).is_empty());
    }

    #[test]
    fn slug_collision_retry_and_csprng_failure() {
        let registry = Registry::default();
        let first = session("first");
        registry
            .allocate_tunnel(Arc::clone(&first), None, ONE, || Ok("collision".into()))
            .unwrap();
        let mut attempts = 0;
        let second = registry
            .allocate_tunnel(session("second"), None, ONE, || {
                attempts += 1;
                Ok(if attempts == 1 { "collision" } else { "fresh" }.into())
            })
            .unwrap();
        assert_eq!(second.subdomain, "fresh");
        assert_eq!(attempts, 2);
        assert!(matches!(
            registry.allocate_tunnel(session("third"), None, ONE, || Ok("collision".into())),
            Err(TunnelInsertError::Slug(SlugError::Collisions))
        ));
        assert!(matches!(
            registry.allocate_tunnel(session("fourth"), None, ONE, || Err(SlugError::Random(
                getrandom::Error::UNSUPPORTED
            ))),
            Err(TunnelInsertError::Slug(SlugError::Random(_)))
        ));
    }

    #[test]
    fn revoke_wins_or_closes_registration_race() {
        for _ in 0..64 {
            let registry = Arc::new(Registry::default());
            let candidate = session("candidate");
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let registering = {
                let registry = Arc::clone(&registry);
                let candidate = Arc::clone(&candidate);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    registry.insert_session(candidate)
                })
            };
            let revoking = {
                let registry = Arc::clone(&registry);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    registry.disconnect_token(10);
                })
            };
            barrier.wait();
            let inserted = registering.join().unwrap();
            revoking.join().unwrap();
            assert!(registry.sessions.lock().unwrap().is_empty());
            if inserted.is_ok() {
                assert!(candidate.cancel.is_cancelled());
            }
            assert!(registry.insert_session(session("late")).is_err());
        }
    }

    #[test]
    fn websocket_and_http_limits_are_independent() {
        let registry = Registry::default();
        let tunnel = registry
            .allocate_tunnel(session("a"), None, ONE, || Ok("slug".into()))
            .unwrap();
        let http = tunnel.try_acquire().unwrap();
        let websocket = tunnel.try_acquire_websocket().unwrap();
        assert_eq!(tunnel.active.load(Ordering::Acquire), 1);
        assert_eq!(tunnel.websocket_active.load(Ordering::Acquire), 1);
        drop(http);
        assert_eq!(tunnel.active.load(Ordering::Acquire), 0);
        assert_eq!(tunnel.websocket_active.load(Ordering::Acquire), 1);
        drop(websocket);
        assert_eq!(tunnel.websocket_active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn tunnel_limit_spans_a_users_sessions_and_spares_others() {
        let registry = Registry::default();
        let limits = vorp_store::UserLimits {
            max_tunnels: 2,
            ..vorp_store::UserLimits::DEFAULT
        };
        let budget = registry.budget(1, Some(limits));
        let laptop = user_session("laptop", 1, Arc::clone(&budget));
        let server = user_session("server", 1, Arc::clone(&budget));
        let named = |s: &Arc<Session>, name: &str| {
            Arc::new(Tunnel::new(Arc::clone(s), name.into(), None, ONE))
        };
        registry.insert_named_tunnel(named(&laptop, "one")).unwrap();
        registry
            .allocate_tunnel(Arc::clone(&server), None, ONE, || Ok("two".into()))
            .unwrap();
        assert!(matches!(
            registry.insert_named_tunnel(named(&server, "three")),
            Err(TunnelInsertError::LimitReached)
        ));
        assert!(matches!(
            registry.allocate_tunnel(Arc::clone(&laptop), None, ONE, || Ok("four".into())),
            Err(TunnelInsertError::LimitReached)
        ));

        let other = user_session("other", 2, registry.budget(2, Some(limits)));
        registry.insert_named_tunnel(named(&other, "five")).unwrap();
        let admin = user_session("admin", 3, registry.budget(3, None));
        for name in ["a1", "a2", "a3", "a4"] {
            registry.insert_named_tunnel(named(&admin, name)).unwrap();
        }

        // Closing one frees a slot; raising the limit live frees another.
        registry.teardown(&laptop);
        registry.insert_named_tunnel(named(&server, "six")).unwrap();
        registry.budget(
            1,
            Some(vorp_store::UserLimits {
                max_tunnels: 3,
                ..limits
            }),
        );
        registry
            .insert_named_tunnel(named(&server, "seven"))
            .unwrap();
        assert_eq!(registry.tunnels_for_user(1).len(), 3);
    }

    #[test]
    fn busy_user_does_not_evict_another_users_traffic() {
        let registry = Registry::default();
        let event = |subdomain: &str| vorp_web::TrafficEvent {
            subdomain: subdomain.into(),
            timestamp_ms: 0,
            method: "GET".into(),
            status: 200,
            bytes_in: 0,
            bytes_out: 0,
        };
        registry.record_traffic(1, event("quiet"));
        for _ in 0..TRAFFIC_EVENTS_PER_USER + 10 {
            registry.record_traffic(2, event("busy"));
        }
        let quiet = registry.recent_traffic(1);
        assert_eq!(quiet.len(), 1);
        assert_eq!(quiet[0].subdomain, "quiet");
        assert_eq!(registry.recent_traffic(2).len(), TRAFFIC_EVENTS_PER_USER);
        assert!(registry.recent_traffic(3).is_empty());
    }
}
