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

use crate::subdomain::SlugError;

pub(crate) type SessionKey = (i64, String);
pub(crate) const DEFAULT_TUNNEL_REQUEST_LIMIT: usize = 128;
pub(crate) const DEFAULT_TUNNEL_WEBSOCKET_LIMIT: usize = 128;

pub(crate) struct Session {
    pub id: String,
    pub key: SessionKey,
    pub token_id: Option<i64>,
    pub user_id: i64,
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

#[derive(Default)]
pub(crate) struct Registry {
    sessions: Mutex<HashMap<SessionKey, Arc<Session>>>,
    revoked_tokens: Mutex<HashSet<i64>>,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
    traffic: Mutex<VecDeque<(i64, vorp_web::TrafficEvent)>>,
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

    pub fn insert_named_tunnel(&self, tunnel: Arc<Tunnel>) -> bool {
        let mut map = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tunnel.session.cancel.is_cancelled() || map.contains_key(&tunnel.subdomain) {
            return false;
        }
        map.insert(tunnel.subdomain.clone(), tunnel);
        true
    }

    pub fn allocate_tunnel(
        &self,
        session: Arc<Session>,
        upstream_hint: Option<String>,
        mut generate: impl FnMut() -> Result<String, SlugError>,
    ) -> Result<Arc<Tunnel>, SlugError> {
        let mut map = self
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for _ in 0..8 {
            if session.cancel.is_cancelled() {
                return Err(SlugError::Collisions);
            }
            let slug = generate()?;
            if let std::collections::hash_map::Entry::Vacant(slot) = map.entry(slug.clone()) {
                let tunnel = Arc::new(Tunnel {
                    session,
                    subdomain: slug,
                    concurrency_limit: DEFAULT_TUNNEL_REQUEST_LIMIT,
                    active: std::sync::atomic::AtomicUsize::new(0),
                    websocket_limit: DEFAULT_TUNNEL_WEBSOCKET_LIMIT,
                    websocket_active: std::sync::atomic::AtomicUsize::new(0),
                    upstream_hint,
                    cancel: CancellationToken::new(),
                });
                slot.insert(Arc::clone(&tunnel));
                return Ok(tunnel);
            }
        }
        Err(SlugError::Collisions)
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
        let mut events = self
            .traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if events.len() == 256 {
            events.pop_front();
        }
        events.push_back((user_id, event));
    }

    pub fn recent_traffic(&self, user_id: i64) -> Vec<vorp_web::TrafficEvent> {
        self.traffic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(owner, _)| *owner == user_id)
            .map(|(_, event)| event.clone())
            .collect()
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

    fn session(id: &str) -> Arc<Session> {
        let (open, _) = mpsc::channel(1);
        Arc::new(Session {
            id: id.into(),
            key: (1, "machine".into()),
            token_id: Some(10),
            user_id: 1,
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
        let make = |s: Arc<Session>| {
            Arc::new(Tunnel {
                session: s,
                subdomain: "app".into(),
                concurrency_limit: 1,
                active: std::sync::atomic::AtomicUsize::new(0),
                websocket_limit: 1,
                websocket_active: std::sync::atomic::AtomicUsize::new(0),
                upstream_hint: None,
                cancel: CancellationToken::new(),
            })
        };
        let old = make(Arc::clone(&old_session));
        let new = make(Arc::clone(&new_session));
        registry.insert_named_tunnel(Arc::clone(&old));
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
            .allocate_tunnel(Arc::clone(&first), None, || Ok("collision".into()))
            .unwrap();
        let mut attempts = 0;
        let second = registry
            .allocate_tunnel(session("second"), None, || {
                attempts += 1;
                Ok(if attempts == 1 { "collision" } else { "fresh" }.into())
            })
            .unwrap();
        assert_eq!(second.subdomain, "fresh");
        assert_eq!(attempts, 2);
        assert!(matches!(
            registry.allocate_tunnel(session("third"), None, || Ok("collision".into())),
            Err(SlugError::Collisions)
        ));
        assert!(matches!(
            registry.allocate_tunnel(session("fourth"), None, || Err(SlugError::Random(
                getrandom::Error::UNSUPPORTED
            ))),
            Err(SlugError::Random(_))
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
            .allocate_tunnel(session("a"), None, || Ok("slug".into()))
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
}
