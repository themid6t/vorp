use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::sse::{Event, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use vorp_store::{BindPolicy, NewSession, NewUser, Repository, RepositoryError, User};
mod access;
mod assets;
mod limits;
mod quota;
mod recovery;
mod reservations;
use access::set_user_access;
use limits::{AuthLimiter, LimitError, MAX_TRAFFIC_STREAMS, traffic_stream_permit};
use quota::{LimitsBody, default_limits, list_users, set_default_limits, set_user_limits};
pub use recovery::{ResetError, reset_password};
use reservations::{
    approve_request, list_requests, list_reservations, reject_request, release, reserve,
};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupMode {
    Open,
    Closed,
}
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub signup_mode: SignupMode,
    pub session_ttl_secs: u64,
    /// Tunnel hostnames are `<subdomain>.<base_domain>`; the dashboard builds links from it.
    pub base_domain: String,
}

/// Implement this with the relay's `disconnect_token`. A failed disconnect is surfaced to the
/// caller and can be retried; revocation is never reported as complete while a live agent remains.
pub trait TokenDisconnect: Send + Sync {
    fn disconnect(
        &self,
        token_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}
#[derive(Clone, Serialize)]
pub struct TunnelView {
    pub subdomain: String,
    pub machine_id: String,
    pub upstream_hint: Option<String>,
    pub active_requests: usize,
}
#[derive(Clone, Serialize)]
pub struct TrafficEvent {
    pub subdomain: String,
    pub timestamp_ms: i64,
    pub method: String,
    pub status: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
}
/// The relay must scope every result and force-close operation to the supplied user ID.
pub trait DashboardRuntime: Send + Sync {
    fn tunnels(
        &self,
        user_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TunnelView>, String>> + Send + '_>>;
    fn close_tunnel(
        &self,
        user_id: i64,
        subdomain: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>>;
    fn recent_traffic(
        &self,
        user_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TrafficEvent>, String>> + Send + '_>>;
    /// Stored limits changed; re-apply them to live sessions.
    fn limits_changed(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}
#[derive(Clone)]
struct AppState {
    repository: Repository,
    config: WebConfig,
    disconnect: Option<Arc<dyn TokenDisconnect>>,
    runtime: Option<Arc<dyn DashboardRuntime>>,
    auth_limiter: Arc<AuthLimiter>,
    traffic_stream_slots: Arc<Semaphore>,
}

#[derive(Debug)]
enum ApiError {
    Unauthorized,
    Forbidden,
    Conflict,
    Invalid(&'static str),
    /// A subdomain name is held by someone; the message says by whom.
    Taken(&'static str),
    /// The account must pick a new password before using anything else.
    PasswordChangeRequired,
    Unavailable,
    TooManyRequests,
    Internal,
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::Invalid(m) => (StatusCode::BAD_REQUEST, m),
            Self::Taken(m) => (StatusCode::CONFLICT, m),
            Self::PasswordChangeRequired => (StatusCode::FORBIDDEN, "password change required"),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "relay disconnect unavailable",
            ),
            Self::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(ErrorBody { error: message })).into_response()
    }
}
impl From<LimitError> for ApiError {
    fn from(error: LimitError) -> Self {
        match error {
            LimitError::Busy => Self::TooManyRequests,
            LimitError::Poisoned => Self::Internal,
        }
    }
}
#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}
impl From<RepositoryError> for ApiError {
    fn from(e: RepositoryError) -> Self {
        match e {
            RepositoryError::Conflict => Self::Conflict,
            RepositoryError::Invalid(m) => Self::Invalid(m),
            RepositoryError::NameTaken(conflict) => Self::Taken(conflict.message()),
            RepositoryError::NotFound => Self::Invalid("not found"),
            RepositoryError::Database(_) | RepositoryError::Random => Self::Internal,
        }
    }
}

pub fn router(repository: Repository, config: WebConfig) -> Router {
    router_with_disconnect(repository, config, None)
}
pub fn router_with_disconnect(
    repository: Repository,
    config: WebConfig,
    disconnect: Option<Arc<dyn TokenDisconnect>>,
) -> Router {
    router_with_runtime(repository, config, disconnect, None)
}
pub fn router_with_runtime(
    repository: Repository,
    config: WebConfig,
    disconnect: Option<Arc<dyn TokenDisconnect>>,
    runtime: Option<Arc<dyn DashboardRuntime>>,
) -> Router {
    let state = AppState {
        repository,
        config,
        disconnect,
        runtime,
        auth_limiter: Arc::new(AuthLimiter::new()),
        traffic_stream_slots: Arc::new(Semaphore::new(MAX_TRAFFIC_STREAMS)),
    };
    Router::new()
        .route("/", get(assets::index))
        .route("/{*path}", get(assets::file))
        .route("/healthz", get(health))
        .route("/api/config", get(public_config))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/signup", post(signup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/password", post(change_password))
        .route("/api/users", post(create_user))
        .route("/api/tokens", get(list_tokens).post(mint_token))
        .route("/api/tokens/{id}/revoke", post(revoke_token))
        .route("/api/reservations", get(list_reservations).post(reserve))
        .route("/api/reservations/{name}", axum::routing::delete(release))
        .route("/api/tunnels", get(list_tunnels))
        .route("/api/tunnels/{name}/close", post(force_close_tunnel))
        .route("/api/traffic/recent", get(recent_traffic))
        .route("/api/traffic/stream", get(stream_traffic))
        .route(
            "/api/admin/limits",
            get(default_limits).put(set_default_limits),
        )
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{id}", axum::routing::put(set_user_access))
        .route("/api/admin/reservation-requests", get(list_requests))
        .route(
            "/api/admin/reservation-requests/{name}/approve",
            post(approve_request),
        )
        .route(
            "/api/admin/reservation-requests/{name}/reject",
            post(reject_request),
        )
        .route(
            "/api/admin/users/{id}/limits",
            axum::routing::put(set_user_limits),
        )
        .with_state(state)
}
async fn list_tunnels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TunnelView>>, ApiError> {
    let user = current_user(&state, &headers).await?;
    let runtime = state.runtime.as_ref().ok_or(ApiError::Unavailable)?;
    Ok(Json(
        runtime
            .tunnels(user.id)
            .await
            .map_err(|_| ApiError::Unavailable)?,
    ))
}
async fn force_close_tunnel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<OkBody>, ApiError> {
    mutating_request(&headers)?;
    let user = current_user(&state, &headers).await?;
    let runtime = state.runtime.as_ref().ok_or(ApiError::Unavailable)?;
    Ok(Json(OkBody {
        ok: runtime
            .close_tunnel(user.id, &name)
            .await
            .map_err(|_| ApiError::Unavailable)?,
    }))
}
async fn recent_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TrafficEvent>>, ApiError> {
    let user = current_user(&state, &headers).await?;
    let runtime = state.runtime.as_ref().ok_or(ApiError::Unavailable)?;
    Ok(Json(
        runtime
            .recent_traffic(user.id)
            .await
            .map_err(|_| ApiError::Unavailable)?,
    ))
}
async fn stream_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError> {
    let user = current_user(&state, &headers).await?;
    let runtime = state.runtime.clone().ok_or(ApiError::Unavailable)?;
    let permit = traffic_stream_permit(&state.traffic_stream_slots).map_err(ApiError::from)?;
    let stream = stream::unfold(
        (
            runtime,
            user.id,
            tokio::time::interval(std::time::Duration::from_secs(2)),
            permit,
        ),
        |(runtime, user_id, mut interval, permit)| async move {
            interval.tick().await;
            let payload = match runtime.recent_traffic(user_id).await {
                Ok(events) => Event::default()
                    .json_data(events)
                    .unwrap_or_else(|_| Event::default().data("[]")),
                Err(_) => Event::default().event("error").data("traffic unavailable"),
            };
            Some((Ok(payload), (runtime, user_id, interval, permit)))
        },
    );
    Ok(Sse::new(stream))
}
#[derive(Serialize)]
struct ConfigBody {
    signup_mode: &'static str,
    needs_bootstrap: bool,
    base_domain: String,
}
/// Public: the login screen needs it before anyone has a session.
async fn public_config(State(state): State<AppState>) -> Result<Json<ConfigBody>, ApiError> {
    let users = state
        .repository
        .user_count()
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ConfigBody {
        signup_mode: match state.config.signup_mode {
            SignupMode::Open => "open",
            SignupMode::Closed => "closed",
        },
        needs_bootstrap: users == 0,
        base_domain: state.config.base_domain.clone(),
    }))
}
async fn health() -> StatusCode {
    StatusCode::OK
}

fn now_ms() -> Result<i64, ApiError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ApiError::Internal)?
            .as_millis(),
    )
    .map_err(|_| ApiError::Internal)
}
fn random_hex<const N: usize>() -> Result<String, ApiError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| ApiError::Internal)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}
async fn hash_password(password: &str) -> Result<String, ApiError> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash_password_blocking(&password))
        .await
        .map_err(|_| ApiError::Internal)?
}
fn hash_password_blocking(password: &str) -> Result<String, ApiError> {
    if password.len() < 12 || password.len() > 1024 {
        return Err(ApiError::Invalid(
            "password must contain 12 to 1024 characters",
        ));
    }
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| ApiError::Internal)?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| ApiError::Internal)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| ApiError::Internal)
}
async fn verify_password(password: &str, hash: &str) -> bool {
    let (password, hash) = (password.to_owned(), hash.to_owned());
    tokio::task::spawn_blocking(move || verify_password_blocking(&password, &hash))
        .await
        .unwrap_or(false)
}
fn verify_password_blocking(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}
/// `__Host-` makes browsers refuse the cookie unless it is `Secure`, `Path=/`
/// and has no `Domain`, so a tunnel at `<name>.<base_domain>` cannot plant one
/// for the dashboard.
const SESSION_COOKIE: &str = "__Host-vorp_session";
/// The session id, or `None` when the session cookie appears more than once
/// (across all `Cookie` headers): a duplicate means one was planted, and
/// picking either would allow session fixation.
fn cookie_session(headers: &HeaderMap) -> Option<&str> {
    let mut ids = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='));
    let id = ids.next()?;
    ids.next().is_none().then_some(id)
}
/// The signed-in user, refusing an account that must first change its password.
async fn current_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let user = session_user(state, headers).await?;
    if user.must_change_password {
        return Err(ApiError::PasswordChangeRequired);
    }
    Ok(user)
}
/// The signed-in user, even one that must change its password. Only `me` and
/// the password change itself use this.
async fn session_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let id = cookie_session(headers).ok_or(ApiError::Unauthorized)?;
    let session = state
        .repository
        .session_by_id(id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::Unauthorized)?;
    state
        .repository
        .user_by_id(session.user_id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::Unauthorized)
}
fn mutating_request(headers: &HeaderMap) -> Result<(), ApiError> {
    // Same-origin fetches set this custom header. Cross-site forms cannot set it.
    if headers.get("x-vorp-csrf").and_then(|v| v.to_str().ok()) == Some("1") {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}
fn session_cookie(id: &str, max_age: u64) -> String {
    format!("{SESSION_COOKIE}={id}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age}")
}
fn expired_cookie() -> String {
    session_cookie("", 0)
}
async fn issue_session(state: &AppState, user_id: i64) -> Result<Response, ApiError> {
    let id = random_hex::<32>()?;
    let ttl_ms = i64::try_from(state.config.session_ttl_secs.saturating_mul(1000))
        .map_err(|_| ApiError::Internal)?;
    state
        .repository
        .create_session(NewSession {
            id: id.clone(),
            user_id,
            expires_at_ms: now_ms()?.checked_add(ttl_ms).ok_or(ApiError::Internal)?,
        })
        .await
        .map_err(ApiError::from)?;
    let mut response = Json(OkBody { ok: true }).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&id, state.config.session_ttl_secs)
            .parse()
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}
#[derive(Serialize)]
struct OkBody {
    ok: bool,
}
#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}
async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Response, ApiError> {
    mutating_request(&headers)?;
    let _auth_permit = state
        .auth_limiter
        .admit(&input.email)
        .map_err(ApiError::from)?;
    let hash = hash_password(&input.password).await?;
    let user = state
        .repository
        .bootstrap_admin(&input.email, &hash)
        .await
        .map_err(ApiError::from)?;
    issue_session(&state, user.id).await
}
async fn signup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Response, ApiError> {
    mutating_request(&headers)?;
    if state.config.signup_mode == SignupMode::Closed {
        return Err(ApiError::Forbidden);
    }
    let _auth_permit = state
        .auth_limiter
        .admit(&input.email)
        .map_err(ApiError::from)?;
    let hash = hash_password(&input.password).await?;
    let user = state
        .repository
        .create_user(NewUser {
            email: input.email,
            password_hash: hash,
            is_admin: false,
            must_change_password: false,
        })
        .await
        .map_err(ApiError::from)?;
    issue_session(&state, user.id).await
}
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Response, ApiError> {
    mutating_request(&headers)?;
    let _auth_permit = state
        .auth_limiter
        .admit(&input.email)
        .map_err(ApiError::from)?;
    let user = state
        .repository
        .user_by_email(&input.email)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::Unauthorized)?;
    if !verify_password(&input.password, &user.password_hash).await {
        return Err(ApiError::Unauthorized);
    }
    issue_session(&state, user.id).await
}
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    mutating_request(&headers)?;
    if let Some(id) = cookie_session(&headers) {
        state
            .repository
            .delete_session(id)
            .await
            .map_err(ApiError::from)?;
    }
    let mut response = Json(OkBody { ok: true }).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        expired_cookie().parse().map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}
#[derive(Serialize)]
struct MeBody {
    id: i64,
    email: String,
    is_admin: bool,
    assigned_subdomain: Option<String>,
}
#[derive(Serialize)]
struct MeWithLimitsBody {
    #[serde(flatten)]
    user: MeBody,
    /// `null` for an admin, who is exempt from every quota.
    limits: Option<LimitsBody>,
    must_change_password: bool,
    can_reserve_directly: bool,
}
async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeWithLimitsBody>, ApiError> {
    let u = session_user(&state, &headers).await?;
    let limits = state
        .repository
        .effective_limits(u.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MeWithLimitsBody {
        user: MeBody {
            id: u.id,
            email: u.email,
            is_admin: u.is_admin,
            assigned_subdomain: u.assigned_subdomain,
        },
        limits: limits.map(LimitsBody::from),
        must_change_password: u.must_change_password,
        can_reserve_directly: u.is_admin || u.can_reserve_directly,
    }))
}
#[derive(Deserialize)]
struct PasswordInput {
    old_password: String,
    new_password: String,
}
async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PasswordInput>,
) -> Result<Response, ApiError> {
    mutating_request(&headers)?;
    let u = session_user(&state, &headers).await?;
    let _auth_permit = state.auth_limiter.admit(&u.email).map_err(ApiError::from)?;
    if !verify_password(&input.old_password, &u.password_hash).await {
        return Err(ApiError::Unauthorized);
    }
    let hash = hash_password(&input.new_password).await?;
    state
        .repository
        .update_password(u.id, &hash, false)
        .await
        .map_err(ApiError::from)?;
    let mut response = Json(OkBody { ok: true }).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        expired_cookie().parse().map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}
async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Json<MeBody>, ApiError> {
    mutating_request(&headers)?;
    let admin = current_user(&state, &headers).await?;
    if !admin.is_admin {
        return Err(ApiError::Forbidden);
    }
    let _auth_permit = state
        .auth_limiter
        .admit(&input.email)
        .map_err(ApiError::from)?;
    let hash = hash_password(&input.password).await?;
    let u = state
        .repository
        .create_user(NewUser {
            email: input.email,
            password_hash: hash,
            is_admin: false,
            // The admin chose this password, so the user replaces it first.
            must_change_password: true,
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MeBody {
        id: u.id,
        email: u.email,
        is_admin: false,
        assigned_subdomain: None,
    }))
}
#[derive(Serialize)]
struct TokenBody {
    id: i64,
    bind_policy: &'static str,
    allowlist: Vec<String>,
    revoked: bool,
}
fn token_body(t: vorp_store::TokenRecord) -> TokenBody {
    TokenBody {
        id: t.id,
        bind_policy: match t.bind_policy {
            BindPolicy::Any => "any",
            BindPolicy::Temporary => "temporary",
            BindPolicy::Reserved => "reserved",
        },
        allowlist: t.allowlist,
        revoked: t.revoked_at_ms.is_some(),
    }
}
async fn list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TokenBody>>, ApiError> {
    let u = current_user(&state, &headers).await?;
    Ok(Json(
        state
            .repository
            .tokens_for_user(u.id)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(token_body)
            .collect(),
    ))
}
#[derive(Deserialize)]
struct MintInput {
    bind_policy: String,
    #[serde(default)]
    allowlist: Vec<String>,
}
#[derive(Serialize)]
struct MintBody {
    token: TokenBody,
    raw_token: String,
}
async fn mint_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<MintInput>,
) -> Result<Json<MintBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    let policy = match input.bind_policy.as_str() {
        "any" => BindPolicy::Any,
        "temporary" => BindPolicy::Temporary,
        "reserved" => BindPolicy::Reserved,
        _ => return Err(ApiError::Invalid("bind policy")),
    };
    let (token, raw_token) = state
        .repository
        .mint_token(u.id, policy, input.allowlist)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MintBody {
        token: token_body(token),
        raw_token,
    }))
}
async fn revoke_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<OkBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    let disconnect = state.disconnect.as_ref().ok_or(ApiError::Unavailable)?;
    let token = state
        .repository
        .token_for_user(id, u.id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::Invalid("token not found"))?;
    if token.revoked_at_ms.is_none() {
        state
            .repository
            .revoke_token(id, u.id)
            .await
            .map_err(ApiError::from)?;
    }
    disconnect
        .disconnect(id)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    Ok(Json(OkBody { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn config() -> WebConfig {
        WebConfig {
            signup_mode: SignupMode::Open,
            session_ttl_secs: 3600,
            base_domain: "vorp.test".into(),
        }
    }
    async fn repo() -> Repository {
        Repository::open(std::path::Path::new(":memory:"))
            .await
            .expect("db")
    }
    fn req(method: Method, path: &str, body: &str, cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("x-vorp-csrf", "1")
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::from(body.to_owned())).expect("request")
    }
    fn session_cookie_from(response: &Response) -> String {
        response
            .headers()
            .get(header::SET_COOKIE)
            .expect("set cookie")
            .to_str()
            .expect("cookie")
            .split(';')
            .next()
            .expect("pair")
            .to_owned()
    }
    #[test]
    fn session_cookie_is_host_prefixed_without_domain() {
        for cookie in [session_cookie("abc", 60), expired_cookie()] {
            assert!(cookie.starts_with("__Host-vorp_session="));
            for attr in ["; HttpOnly", "; Secure", "; SameSite=Lax", "; Path=/;"] {
                assert!(cookie.contains(attr), "{cookie} lacks {attr}");
            }
            assert!(!cookie.to_ascii_lowercase().contains("domain"));
        }
        assert!(expired_cookie().ends_with("Max-Age=0"));
    }
    #[test]
    fn duplicated_session_cookie_is_unauthenticated() {
        let cases: [(&[&str], Option<&str>); 6] = [
            (&["__Host-vorp_session=abc"], Some("abc")),
            (&["a=1; __Host-vorp_session=abc; b=2"], Some("abc")),
            (&["vorp_session=evil; __Host-vorp_session=abc"], Some("abc")),
            (&["__Host-vorp_session=evil; __Host-vorp_session=abc"], None),
            (
                &["__Host-vorp_session=evil", "__Host-vorp_session=abc"],
                None,
            ),
            (&["vorp_session=abc"], None),
        ];
        for (cookies, expected) in cases {
            let mut headers = HeaderMap::new();
            for cookie in cookies {
                headers.append(header::COOKIE, cookie.parse().expect("cookie"));
            }
            assert_eq!(cookie_session(&headers), expected, "{cookies:?}");
        }
    }
    async fn account(router: &Router, email: &str) -> String {
        let body = serde_json::json!({"email":email,"password":"long enough password"}).to_string();
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/signup", &body, None))
            .await
            .expect("signup");
        assert_eq!(response.status(), StatusCode::OK);
        session_cookie_from(&response)
    }
    struct DisconnectCounter(AtomicUsize);
    impl TokenDisconnect for DisconnectCounter {
        fn disconnect(
            &self,
            _token_id: i64,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }
    /// Counts `limits_changed` calls; every other hook is empty.
    #[derive(Default)]
    struct EmptyRuntime(AtomicUsize);
    impl DashboardRuntime for EmptyRuntime {
        fn limits_changed(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn tunnels(
            &self,
            _user_id: i64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<TunnelView>, String>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn close_tunnel(
            &self,
            _user_id: i64,
            _subdomain: &str,
        ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
            Box::pin(async { Ok(false) })
        }
        fn recent_traffic(
            &self,
            _user_id: i64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<TrafficEvent>, String>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }
    }
    #[tokio::test]
    async fn password_hash_is_argon2id() {
        let hash = hash_password("long enough password").await.expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("long enough password", &hash).await);
        assert!(!verify_password("different password", &hash).await);
    }
    #[test]
    fn csrf_header_is_required() {
        let mut headers = HeaderMap::new();
        assert!(mutating_request(&headers).is_err());
        headers.insert("x-vorp-csrf", "1".parse().expect("header"));
        assert!(mutating_request(&headers).is_ok());
    }
    #[tokio::test]
    async fn api_requires_session_and_scopes_tokens_and_reservations() {
        let repository = repo().await;
        let router = router(repository.clone(), config());
        for path in [
            "/api/me",
            "/api/tokens",
            "/api/reservations",
            "/api/traffic/recent",
        ] {
            let response = router
                .clone()
                .oneshot(req(Method::GET, path, "", None))
                .await
                .expect("request");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let alice = account(&router, "alice@example.test").await;
        let bob = account(&router, "bob@example.test").await;
        let alice_id = repository
            .user_by_email("alice@example.test")
            .await
            .expect("lookup")
            .expect("alice")
            .id;
        repository
            .set_user_access(alice_id, false, true)
            .await
            .expect("direct reservation");
        let response = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/reservations",
                r#"{"name":"alice-test"}"#,
                Some(&alice),
            ))
            .await
            .expect("reserve");
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/tokens",
                r#"{"bind_policy":"reserved","allowlist":["alice-test"]}"#,
                Some(&alice),
            ))
            .await
            .expect("mint");
        assert_eq!(response.status(), StatusCode::OK);
        for path in ["/api/tokens", "/api/reservations"] {
            let response = router
                .clone()
                .oneshot(req(Method::GET, path, "", Some(&bob)))
                .await
                .expect("list");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 16 * 1024)
                .await
                .expect("body");
            assert_eq!(&bytes[..], b"[]", "{path}");
        }
        let response = router
            .clone()
            .oneshot(req(
                Method::DELETE,
                "/api/reservations/alice-test",
                "",
                Some(&bob),
            ))
            .await
            .expect("release");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        assert_eq!(&bytes[..], b"{\"ok\":false}");
    }
    #[tokio::test]
    async fn logout_and_password_change_invalidate_sessions() {
        let repository = repo().await;
        let router = router(repository, config());
        let cookie = account(&router, "alice@example.test").await;
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/logout", "", Some(&cookie)))
            .await
            .expect("logout");
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(req(Method::GET, "/api/me", "", Some(&cookie)))
            .await
            .expect("me");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let login = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/login",
                r#"{"email":"alice@example.test","password":"long enough password"}"#,
                None,
            ))
            .await
            .expect("login");
        let cookie = session_cookie_from(&login);
        let response = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/password",
                r#"{"old_password":"long enough password","new_password":"a new long password"}"#,
                Some(&cookie),
            ))
            .await
            .expect("password");
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(req(Method::GET, "/api/me", "", Some(&cookie)))
            .await
            .expect("me");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn bootstrap_is_one_shot_even_under_race() {
        let repository = repo().await;
        let router = router(repository, config());
        let body = r#"{"email":"admin@example.test","password":"long enough password"}"#;
        let (a, b) = tokio::join!(
            router
                .clone()
                .oneshot(req(Method::POST, "/api/bootstrap", body, None)),
            router
                .clone()
                .oneshot(req(Method::POST, "/api/bootstrap", body, None))
        );
        let statuses = [a.expect("first").status(), b.expect("second").status()];
        assert!(statuses.contains(&StatusCode::OK));
        assert!(statuses.contains(&StatusCode::CONFLICT));
    }
    #[tokio::test]
    async fn revoke_invokes_callback_and_retry_is_safe() {
        let repository = repo().await;
        let counter = Arc::new(DisconnectCounter(AtomicUsize::new(0)));
        let router = router_with_disconnect(repository, config(), Some(counter.clone()));
        let cookie = account(&router, "alice@example.test").await;
        let response = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/tokens",
                r#"{"bind_policy":"any"}"#,
                Some(&cookie),
            ))
            .await
            .expect("mint");
        let bytes = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let id = json["token"]["id"].as_i64().expect("id");
        let path = format!("/api/tokens/{id}/revoke");
        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(req(Method::POST, &path, "", Some(&cookie)))
                .await
                .expect("revoke");
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(counter.0.load(Ordering::SeqCst), 2);
    }
    /// Bootstraps an admin and returns its session cookie.
    async fn admin(router: &Router) -> String {
        let response = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/bootstrap",
                r#"{"email":"admin@example.test","password":"long enough password"}"#,
                None,
            ))
            .await
            .expect("bootstrap");
        assert_eq!(response.status(), StatusCode::OK);
        session_cookie_from(&response)
    }
    async fn login(router: &Router, email: &str, password: &str) -> String {
        let body = serde_json::json!({"email":email,"password":password}).to_string();
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/login", &body, None))
            .await
            .expect("login");
        assert_eq!(response.status(), StatusCode::OK);
        session_cookie_from(&response)
    }
    #[tokio::test]
    async fn closed_signup_is_refused() {
        let router = router(
            repo().await,
            WebConfig {
                signup_mode: SignupMode::Closed,
                ..config()
            },
        );
        let body = r#"{"email":"new@example.test","password":"long enough password"}"#;
        let (status, _) = json(&router, req(Method::POST, "/api/signup", body, None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn admin_created_user_must_change_password_first() {
        let router = router(repo().await, config());
        let admin = admin(&router).await;
        let body = r#"{"email":"bob@example.test","password":"chosen by admin"}"#;
        let (status, _) = json(&router, req(Method::POST, "/api/users", body, Some(&admin))).await;
        assert_eq!(status, StatusCode::OK);
        let bob = login(&router, "bob@example.test", "chosen by admin").await;
        let (status, me) = json(&router, req(Method::GET, "/api/me", "", Some(&bob))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["must_change_password"], true);
        for path in ["/api/tokens", "/api/reservations", "/api/tunnels"] {
            let (status, body) = json(&router, req(Method::GET, path, "", Some(&bob))).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
            assert_eq!(body["error"], "password change required", "{path}");
        }
        let change = r#"{"old_password":"chosen by admin","new_password":"bob's own password"}"#;
        let (status, _) = json(
            &router,
            req(Method::POST, "/api/password", change, Some(&bob)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let bob = login(&router, "bob@example.test", "bob's own password").await;
        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&bob))).await;
        assert_eq!(me["must_change_password"], false);
        let (status, _) = json(&router, req(Method::GET, "/api/tokens", "", Some(&bob))).await;
        assert_eq!(status, StatusCode::OK);
    }
    #[tokio::test]
    async fn reservations_need_approval_unless_allowed_directly() {
        let router = router(repo().await, config());
        let admin = admin(&router).await;
        let alice = account(&router, "alice@example.test").await;
        let bob = account(&router, "bob@example.test").await;
        let reserve = |cookie: &str, name: &str| {
            req(
                Method::POST,
                "/api/reservations",
                &serde_json::json!({ "name": name }).to_string(),
                Some(cookie),
            )
        };
        let (status, body) = json(&router, reserve(&alice, "Demo")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"name":"demo","status":"pending"}));
        let (status, body) = json(&router, reserve(&bob, "demo")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "another user has already requested this name"
        );
        let (status, _) = json(
            &router,
            req(
                Method::GET,
                "/api/admin/reservation-requests",
                "",
                Some(&alice),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, pending) = json(
            &router,
            req(
                Method::GET,
                "/api/admin/reservation-requests",
                "",
                Some(&admin),
            ),
        )
        .await;
        assert_eq!(pending[0]["email"], "alice@example.test");
        let approve = "/api/admin/reservation-requests/demo/approve";
        let (status, _) = json(&router, req(Method::POST, approve, "", Some(&admin))).await;
        assert_eq!(status, StatusCode::OK);
        let (_, mine) = json(
            &router,
            req(Method::GET, "/api/reservations", "", Some(&alice)),
        )
        .await;
        assert_eq!(
            mine,
            serde_json::json!([{"name":"demo","status":"reserved"}])
        );
        let (status, body) = json(&router, reserve(&bob, "demo")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "this name is already reserved by another user"
        );

        let (_, users) = json(
            &router,
            req(Method::GET, "/api/admin/users", "", Some(&admin)),
        )
        .await;
        let bob_id = users
            .as_array()
            .expect("users")
            .iter()
            .find(|u| u["email"] == "bob@example.test")
            .expect("bob")["id"]
            .clone();
        let access = format!("/api/admin/users/{bob_id}");
        let body = r#"{"is_admin":false,"can_reserve_directly":true}"#;
        let (status, _) = json(&router, req(Method::PUT, &access, body, Some(&admin))).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = json(&router, reserve(&bob, "fast")).await;
        assert_eq!(body["status"], "reserved");
    }
    #[tokio::test]
    async fn admin_promotes_users_but_not_demotes_self() {
        let runtime = Arc::new(EmptyRuntime::default());
        let router = router_with_runtime(repo().await, config(), None, Some(runtime.clone()));
        let admin = admin(&router).await;
        let bob = account(&router, "bob@example.test").await;
        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&bob))).await;
        let promote = format!("/api/admin/users/{}", me["id"]);
        let body = r#"{"is_admin":true,"can_reserve_directly":false}"#;
        let (status, _) = json(&router, req(Method::PUT, &promote, body, Some(&bob))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = json(&router, req(Method::PUT, &promote, body, Some(&admin))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            runtime.0.load(Ordering::SeqCst),
            1,
            "live limits re-applied"
        );
        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&bob))).await;
        assert_eq!(
            (me["is_admin"].clone(), me["limits"].clone()),
            (true.into(), serde_json::Value::Null)
        );
        let (_, admin_me) = json(&router, req(Method::GET, "/api/me", "", Some(&admin))).await;
        let demote_self = format!("/api/admin/users/{}", admin_me["id"]);
        let body = r#"{"is_admin":false,"can_reserve_directly":false}"#;
        let (status, _) = json(&router, req(Method::PUT, &demote_self, body, Some(&admin))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The promoted admin may demote the first one.
        let (status, _) = json(&router, req(Method::PUT, &demote_self, body, Some(&bob))).await;
        assert_eq!(status, StatusCode::OK);
    }
    #[tokio::test]
    async fn login_rate_limit_returns_429() {
        let router = router(repo().await, config());
        let body = r#"{"email":"unknown@example.test","password":"long enough password"}"#;
        for _ in 0..10 {
            let response = router
                .clone()
                .oneshot(req(Method::POST, "/api/login", body, None))
                .await
                .expect("login");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/login", body, None))
            .await
            .expect("limited");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn signup_rate_limit_returns_429_before_hashing() {
        let router = router(repo().await, config());
        let body = r#"{"email":"new@example.test","password":"short"}"#;
        for _ in 0..10 {
            let response = router
                .clone()
                .oneshot(req(Method::POST, "/api/signup", body, None))
                .await
                .expect("signup");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/signup", body, None))
            .await
            .expect("limited");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn traffic_stream_cap_releases_on_disconnect() {
        let router = router_with_runtime(
            repo().await,
            config(),
            None,
            Some(Arc::new(EmptyRuntime::default())),
        );
        let cookie = account(&router, "viewer@example.test").await;
        let mut streams = Vec::new();
        for _ in 0..MAX_TRAFFIC_STREAMS {
            let response = router
                .clone()
                .oneshot(req(Method::GET, "/api/traffic/stream", "", Some(&cookie)))
                .await
                .expect("stream");
            assert_eq!(response.status(), StatusCode::OK);
            streams.push(response);
        }
        let full = router
            .clone()
            .oneshot(req(Method::GET, "/api/traffic/stream", "", Some(&cookie)))
            .await
            .expect("full");
        assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
        streams.pop();
        let reopened = router
            .clone()
            .oneshot(req(Method::GET, "/api/traffic/stream", "", Some(&cookie)))
            .await
            .expect("reopened");
        assert_eq!(reopened.status(), StatusCode::OK);
    }
    async fn json(router: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }
    #[tokio::test]
    async fn admin_sets_limits_and_users_see_them() {
        let runtime = Arc::new(EmptyRuntime::default());
        let router = router_with_runtime(repo().await, config(), None, Some(runtime.clone()));
        let credentials =
            serde_json::json!({"email":"admin@example.test","password":"long enough password"})
                .to_string();
        let response = router
            .clone()
            .oneshot(req(Method::POST, "/api/bootstrap", &credentials, None))
            .await
            .expect("bootstrap");
        assert_eq!(response.status(), StatusCode::OK);
        let admin = session_cookie_from(&response);
        let alice = account(&router, "alice@example.test").await;

        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&alice))).await;
        assert_eq!(me["limits"]["max_tunnels"], 3);
        let alice_id = me["id"].as_i64().expect("id");
        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&admin))).await;
        assert!(me["limits"].is_null(), "admins are exempt");

        let defaults =
            r#"{"max_tunnels":5,"bandwidth_bytes_per_sec":1000,"max_concurrent_requests":8}"#;
        let overrides_path = format!("/api/admin/users/{alice_id}/limits");
        for (method, path, body) in [
            (Method::GET, "/api/admin/limits", ""),
            (Method::PUT, "/api/admin/limits", defaults),
            (Method::GET, "/api/admin/users", ""),
            (Method::PUT, overrides_path.as_str(), r#"{"max_tunnels":9}"#),
        ] {
            let (status, _) = json(&router, req(method, path, body, Some(&alice))).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        }
        let mut no_csrf = req(Method::PUT, "/api/admin/limits", defaults, Some(&admin));
        no_csrf.headers_mut().remove("x-vorp-csrf");
        let (status, _) = json(&router, no_csrf).await;
        assert_ne!(status, StatusCode::OK);
        assert_eq!(runtime.0.load(Ordering::SeqCst), 0);

        let (status, _) = json(
            &router,
            req(Method::PUT, "/api/admin/limits", defaults, Some(&admin)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = json(
            &router,
            req(
                Method::PUT,
                &overrides_path,
                r#"{"max_tunnels":1}"#,
                Some(&admin),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            runtime.0.load(Ordering::SeqCst),
            2,
            "live sessions are refreshed"
        );

        let (_, me) = json(&router, req(Method::GET, "/api/me", "", Some(&alice))).await;
        assert_eq!(me["limits"]["max_tunnels"], 1);
        assert_eq!(me["limits"]["max_concurrent_requests"], 8);
        let (_, users) = json(
            &router,
            req(Method::GET, "/api/admin/users", "", Some(&admin)),
        )
        .await;
        assert!(users[0]["effective"].is_null());
        assert_eq!(users[1]["overrides"]["max_tunnels"], 1);
        assert!(users[1]["overrides"]["bandwidth_bytes_per_sec"].is_null());
        assert_eq!(users[1]["effective"]["bandwidth_bytes_per_sec"], 1000);

        for (path, body) in [
            (overrides_path.as_str(), r#"{"max_tunnels":0}"#),
            ("/api/admin/users/9999/limits", r#"{"max_tunnels":2}"#),
            (
                "/api/admin/limits",
                r#"{"max_tunnels":1,"bandwidth_bytes_per_sec":0,"max_concurrent_requests":1}"#,
            ),
        ] {
            let (status, _) = json(&router, req(Method::PUT, path, body, Some(&admin))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {body}");
        }
    }
    #[tokio::test]
    async fn public_config_reports_bootstrap_until_first_user() {
        let router = router(repo().await, config());
        let get = |path: &str| {
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request")
        };
        let (status, body) = json(&router, get("/api/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({"signup_mode":"open","needs_bootstrap":true,"base_domain":"vorp.test"})
        );
        account(&router, "alice@example.test").await;
        let (_, body) = json(&router, get("/api/config")).await;
        assert_eq!(body["needs_bootstrap"], false);
    }
}
