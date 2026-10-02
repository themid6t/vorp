use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use vorp_agent::AgentConfig;
use vorp_relay::{EdgeLimits, RelayConfig, SignupMode, TlsConfig};

#[derive(Parser)]
#[command(name = "vorp", about = "Self-hosted reverse tunnels")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    agent: AgentArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Run the relay.
    Serve(Box<ServeArgs>),
    /// Save an agent token read from standard input.
    Authtoken(TokenArgs),
}

#[derive(Args)]
struct TokenArgs {
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SignupArg {
    Open,
    Invite,
    Closed,
}

impl From<SignupArg> for SignupMode {
    fn from(value: SignupArg) -> Self {
        match value {
            SignupArg::Open => Self::Open,
            SignupArg::Invite => Self::Invite,
            SignupArg::Closed => Self::Closed,
        }
    }
}

#[derive(Args)]
struct AgentArgs {
    #[arg(long, default_value = "localhost")]
    relay_host: String,

    /// Dial this address instead of resolving `--relay-host` on port 443.
    #[arg(long)]
    relay_addr: Option<SocketAddr>,

    #[arg(long)]
    ca_cert: Option<PathBuf>,

    #[arg(long, env = "VORP_TOKEN", hide_env_values = true)]
    token: Option<String>,

    #[arg(long)]
    token_file: Option<PathBuf>,

    #[arg(long)]
    upstream: Option<String>,

    #[arg(long = "subdomain")]
    subdomains: Vec<String>,

    #[arg(long)]
    allow_remote_targets: bool,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, default_value = "0.0.0.0:443")]
    listen: SocketAddr,

    #[arg(long)]
    base_domain: String,

    #[arg(long)]
    dashboard_host: Option<String>,

    #[arg(long, default_value = "vorp.sqlite3")]
    database_path: PathBuf,

    #[arg(long, value_enum, default_value = "closed")]
    signup: SignupArg,

    /// Concurrent TLS connections, including handshakes.
    #[arg(long, default_value_t = EdgeLimits::default().max_connections)]
    max_connections: usize,

    /// Concurrent TLS connections from one IP (agents and browsers combined).
    #[arg(long, default_value_t = EdgeLimits::default().max_connections_per_ip)]
    max_connections_per_ip: usize,

    /// HTTP requests per second per IP; the burst is twice this.
    #[arg(long, default_value_t = EdgeLimits::default().requests_per_second_per_ip)]
    rate_limit_rps: u64,

    /// Concurrent proxied HTTP requests across all tunnels.
    #[arg(long, default_value_t = EdgeLimits::default().max_requests)]
    max_requests: usize,

    /// Concurrent WebSockets across all tunnels.
    #[arg(long, default_value_t = EdgeLimits::default().max_websockets)]
    max_websockets: usize,

    /// Concurrent proxied HTTP requests per tunnel.
    #[arg(long, default_value_t = EdgeLimits::default().tunnel_requests)]
    tunnel_requests: usize,

    /// Concurrent WebSockets per tunnel.
    #[arg(long, default_value_t = EdgeLimits::default().tunnel_websockets)]
    tunnel_websockets: usize,

    /// Seconds to wait for an upstream's response head (resets on upload progress).
    #[arg(long, default_value_t = EdgeLimits::default().response_timeout.as_secs())]
    response_timeout_secs: u64,

    #[arg(long, requires = "tls_key", conflicts_with = "dev_self_signed")]
    tls_cert: Option<PathBuf>,

    #[arg(long, requires = "tls_cert", conflicts_with = "dev_self_signed")]
    tls_key: Option<PathBuf>,

    /// Generate a temporary certificate for local development.
    #[arg(long)]
    dev_self_signed: bool,

    #[arg(long, requires = "dev_self_signed")]
    dev_cert_out: Option<PathBuf>,

    #[arg(long, env = "VORP_DEV_TOKEN", hide_env_values = true)]
    dev_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve(args)) => serve(*args).await,
        Some(Command::Authtoken(args)) => save_token(args),
        None => run_agent(cli.agent).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let tls = match (args.tls_cert, args.tls_key, args.dev_self_signed) {
        (Some(cert), Some(key), false) => TlsConfig::Files { cert, key },
        (None, None, true) => TlsConfig::SelfSigned {
            cert_output: args
                .dev_cert_out
                .context("--dev-self-signed requires --dev-cert-out")?,
        },
        _ => bail!("provide --tls-cert and --tls-key, or --dev-self-signed --dev-cert-out"),
    };
    let config = RelayConfig {
        listen: args.listen,
        dashboard_host: args
            .dashboard_host
            .unwrap_or_else(|| args.base_domain.clone()),
        base_domain: args.base_domain,
        database_path: args.database_path,
        tls,
        signup_mode: args.signup.into(),
        dev_token: args.dev_token,
        limits: EdgeLimits {
            max_connections: args.max_connections,
            max_connections_per_ip: args.max_connections_per_ip,
            requests_per_second_per_ip: args.rate_limit_rps,
            max_requests: args.max_requests,
            max_websockets: args.max_websockets,
            tunnel_requests: args.tunnel_requests,
            tunnel_websockets: args.tunnel_websockets,
            response_timeout: std::time::Duration::from_secs(args.response_timeout_secs),
        },
    };
    vorp_relay::serve_until(config, shutdown_on_signal()?)
        .await
        .context("relay stopped")
}

async fn run_agent(args: AgentArgs) -> Result<()> {
    let token = match args.token {
        Some(token) => token,
        None => {
            let path = args.token_file.unwrap_or(default_token_path()?);
            std::fs::read_to_string(&path)
                .with_context(|| format!("read agent token from {}", path.display()))?
                .trim()
                .to_owned()
        }
    };
    if token.is_empty() {
        bail!("agent token is empty");
    }
    let upstream = args.upstream.context("--upstream is required")?;
    let requested_subdomains = if args.subdomains.is_empty() {
        vec![None]
    } else {
        args.subdomains.into_iter().map(Some).collect()
    };
    let config = AgentConfig {
        relay_host: args.relay_host,
        relay_addr: args.relay_addr,
        ca_cert: args.ca_cert,
        token,
        upstream,
        requested_subdomains,
        allow_remote_targets: args.allow_remote_targets,
    };
    config
        .validate()
        .context("agent configuration is invalid")?;
    vorp_agent::run_until(config, shutdown_on_signal()?)
        .await
        .context("agent stopped")
}

/// Cancels the returned token on Ctrl-C or SIGTERM (what systemd and
/// Kubernetes send), so listeners stop accepting and in-flight work drains.
fn shutdown_on_signal() -> Result<CancellationToken> {
    let shutdown = CancellationToken::new();
    let cancel = shutdown.clone();
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("install SIGTERM handler")?;
    tokio::spawn(async move {
        #[cfg(unix)]
        let terminated = terminate.recv();
        #[cfg(not(unix))]
        let terminated = std::future::pending::<Option<()>>();
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::warn!(error = %error, "Ctrl-C handler failed");
                }
            }
            _ = terminated => {}
            _ = cancel.cancelled() => return,
        }
        tracing::info!("shutdown signal received");
        cancel.cancel();
    });
    Ok(shutdown)
}

fn default_token_path() -> Result<PathBuf> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("no config directory; pass --token-file or set VORP_TOKEN")?;
    Ok(config_dir.join("vorp").join("authtoken"))
}

fn save_token(args: TokenArgs) -> Result<()> {
    use std::io::{Read, Write};

    let path = args.token_file.unwrap_or(default_token_path()?);
    let mut token = String::new();
    std::io::stdin()
        .take(4096)
        .read_to_string(&mut token)
        .context("read agent token from standard input")?;
    let token = token.trim();
    if token.is_empty() || token.len() >= 4096 {
        bail!("agent token must contain 1 to 4095 bytes");
    }
    let directory = path
        .parent()
        .context("token file has no parent directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create token directory {}", directory.display()))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("open token file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure token file {}", path.display()))?;
    }
    file.write_all(token.as_bytes())
        .with_context(|| format!("write token file {}", path.display()))?;
    tracing::info!(path = %path.display(), "agent token stored");
    Ok(())
}
