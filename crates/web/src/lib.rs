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

pub fn router(_repository: vorp_store::Repository, _config: WebConfig) -> axum::Router {
    todo!("web workstream implements axum router")
}
