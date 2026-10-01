use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone)]
pub struct RelayConfig {
    pub listen: SocketAddr,
    pub base_domain: String,
    pub dashboard_host: String,
    pub database_path: PathBuf,
    pub tls: TlsConfig,
}

#[derive(Clone)]
pub enum TlsConfig {
    Files {
        cert: PathBuf,
        key: PathBuf,
    },
    Acme(AcmeConfig),
    /// M1 local development only.
    SelfSigned,
}

#[derive(Clone)]
pub struct AcmeConfig {
    pub account_email: String,
    pub cloudflare_token: String,
    pub storage_path: PathBuf,
}
