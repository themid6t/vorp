use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::sse::{Event, Sse},
    response::{Html, IntoResponse, Response},
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
mod limits;
mod quota;
use limits::{AuthLimiter, LimitError, MAX_TRAFFIC_STREAMS, traffic_stream_permit};
use quota::{LimitsBody, default_limits, list_users, set_default_limits, set_user_limits};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupMode {
    Open,
    Invite,
    Closed,
}
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub signup_mode: SignupMode,
    pub session_ttl_secs: u64,
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
        .route("/", get(index))
        .route("/healthz", get(health))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/signup", post(signup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/password", post(change_password))
        .route("/api/users", post(create_user))
        .route("/api/invites", post(mint_invite))
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
async fn index() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
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
fn cookie_session(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| pair.trim().strip_prefix("vorp_session="))
}
async fn current_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
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
    format!("vorp_session={id}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age}")
}
fn expired_cookie() -> &'static str {
    "vorp_session=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0"
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
    #[serde(default)]
    invite_code: Option<String>,
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
    if state.config.signup_mode == SignupMode::Invite && input.invite_code.is_none() {
        return Err(ApiError::Invalid("invite code required"));
    }
    let _auth_permit = state
        .auth_limiter
        .admit(&input.email)
        .map_err(ApiError::from)?;
    let hash = hash_password(&input.password).await?;
    let user_input = NewUser {
        email: input.email,
        password_hash: hash,
        is_admin: false,
    };
    let user = match state.config.signup_mode {
        SignupMode::Open => state.repository.create_user(user_input).await,
        SignupMode::Invite => {
            state
                .repository
                .create_user_with_invite(
                    user_input,
                    input
                        .invite_code
                        .as_deref()
                        .ok_or(ApiError::Invalid("invite code required"))?,
                )
                .await
        }
        SignupMode::Closed => return Err(ApiError::Forbidden),
    }
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
}
async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeWithLimitsBody>, ApiError> {
    let u = current_user(&state, &headers).await?;
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
    let u = current_user(&state, &headers).await?;
    let _auth_permit = state.auth_limiter.admit(&u.email).map_err(ApiError::from)?;
    if !verify_password(&input.old_password, &u.password_hash).await {
        return Err(ApiError::Unauthorized);
    }
    let hash = hash_password(&input.new_password).await?;
    state
        .repository
        .update_password(u.id, &hash)
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
struct InviteBody {
    invite_code: String,
}
async fn mint_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<InviteBody>, ApiError> {
    mutating_request(&headers)?;
    let admin = current_user(&state, &headers).await?;
    if !admin.is_admin {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(InviteBody {
        invite_code: state
            .repository
            .mint_invite(admin.id)
            .await
            .map_err(ApiError::from)?,
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
#[derive(Serialize)]
struct ReservationBody {
    name: String,
}
async fn list_reservations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ReservationBody>>, ApiError> {
    let u = current_user(&state, &headers).await?;
    Ok(Json(
        state
            .repository
            .reservations_for_user(u.id)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|r| ReservationBody { name: r.name })
            .collect(),
    ))
}
#[derive(Deserialize)]
struct ReserveInput {
    name: String,
}
async fn reserve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ReserveInput>,
) -> Result<Json<ReservationBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    let r = state
        .repository
        .reserve_subdomain(&input.name, u.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ReservationBody { name: r.name }))
}
async fn release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<OkBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    Ok(Json(OkBody {
        ok: state
            .repository
            .release_subdomain(&name, u.id)
            .await
            .map_err(ApiError::from)?,
    }))
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
        let router = router(repository, config());
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
    #[tokio::test]
    async fn invite_signup_requires_admin_issued_single_use_code() {
        let repository = repo().await;
        let router = router(
            repository,
            WebConfig {
                signup_mode: SignupMode::Invite,
                session_ttl_secs: 3600,
            },
        );
        let bootstrap = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/bootstrap",
                r#"{"email":"admin@example.test","password":"long enough password"}"#,
                None,
            ))
            .await
            .expect("bootstrap");
        assert_eq!(bootstrap.status(), StatusCode::OK);
        let cookie = session_cookie_from(&bootstrap);
        let denied = router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/signup",
                r#"{"email":"new@example.test","password":"long enough password"}"#,
                None,
            ))
            .await
            .expect("signup");
        assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
        let issued = router
            .clone()
            .oneshot(req(Method::POST, "/api/invites", "", Some(&cookie)))
            .await
            .expect("invite");
        assert_eq!(issued.status(), StatusCode::OK);
        let bytes = to_bytes(issued.into_body(), 16 * 1024).await.expect("body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let code = value["invite_code"].as_str().expect("code");
        let body=serde_json::json!({"email":"new@example.test","password":"long enough password","invite_code":code}).to_string();
        let signup = router
            .clone()
            .oneshot(req(Method::POST, "/api/signup", &body, None))
            .await
            .expect("signup");
        assert_eq!(signup.status(), StatusCode::OK);
        let second = router
            .clone()
            .oneshot(req(Method::POST, "/api/signup", &body, None))
            .await
            .expect("second");
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
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
}
