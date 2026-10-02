use std::{net::SocketAddr, path::PathBuf, time::Duration};
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
    /// Concurrent proxied HTTP requests across all tunnels.
    pub max_requests: usize,
    /// Concurrent upgraded WebSockets across all tunnels.
    pub max_websockets: usize,
    /// Concurrent proxied HTTP requests per tunnel.
    pub tunnel_requests: usize,
    /// Concurrent upgraded WebSockets per tunnel.
    pub tunnel_websockets: usize,
    /// Longest wait for an upstream's response head with no upload progress.
    pub response_timeout: Duration,
}

impl Default for EdgeLimits {
    fn default() -> Self {
        Self {
            max_connections: 1024,
            max_connections_per_ip: 64,
            requests_per_second_per_ip: 200,
            max_requests: 256,
            max_websockets: 128,
            tunnel_requests: 128,
            tunnel_websockets: 128,
            response_timeout: Duration::from_secs(30),
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
