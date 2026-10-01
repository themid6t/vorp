use std::{net::SocketAddr, path::PathBuf};
use vorp_web::SignupMode;

#[derive(Clone)]
pub struct RelayConfig {
    pub listen: SocketAddr,
    pub base_domain: String,
    pub dashboard_host: String,
    pub database_path: PathBuf,
    pub tls: TlsConfig,
    pub signup_mode: SignupMode,
    /// M1 local development credential. File TLS requires a loopback listener.
    pub dev_token: Option<String>,
}

#[derive(Clone)]
pub enum TlsConfig {
    Files {
        cert: PathBuf,
        key: PathBuf,
    },
    Acme(AcmeConfig),
    /// M1 local development only.
    SelfSigned {
        cert_output: PathBuf,
    },
}

#[derive(Clone)]
pub struct AcmeConfig {
    pub account_email: String,
    pub cloudflare_token: String,
    pub storage_path: PathBuf,
}
