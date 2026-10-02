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
    pub limits: EdgeLimits,
}

/// Edge protections the relay applies itself, since nothing sits in front of it.
#[derive(Clone, Copy, Debug)]
pub struct EdgeLimits {
    /// Concurrent TLS connections, including handshakes.
    pub max_connections: usize,
    /// Concurrent TLS connections from one peer IP; agents and HTTP clients
    /// behind one NAT share it.
    pub max_connections_per_ip: usize,
    /// Sustained HTTP requests per second per peer IP; the burst is twice this.
    pub requests_per_second_per_ip: u64,
}

impl Default for EdgeLimits {
    fn default() -> Self {
        Self {
            max_connections: 1024,
            max_connections_per_ip: 64,
            requests_per_second_per_ip: 200,
        }
    }
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
