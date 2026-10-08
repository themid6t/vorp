//! Command-line definitions. Every setting flag is optional with no clap
//! default, so the merge step can tell "not given" from "given"; defaults are
//! applied there, after the environment and the config file.

use std::{net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::config::{AgentLayer, DevOptions, LimitsLayer, RelayLayer, Signup, TlsLayer};

#[derive(Parser)]
#[command(name = "vorp", version, about = "Self-hosted reverse tunnels")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    #[command(flatten)]
    pub(crate) agent: AgentArgs,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Run the relay.
    Serve(Box<ServeArgs>),
    /// Save an agent token read from standard input.
    Authtoken(TokenArgs),
    /// Save an agent token and the relay's name in the agent config.
    Login(LoginArgs),
    /// Show the config file path or the effective settings.
    #[command(subcommand)]
    Config(Box<ConfigCommand>),
    /// Account recovery, run on the relay host against its database.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Subcommand)]
pub(crate) enum AdminCommand {
    /// Replace an account's password with a new random one and end its
    /// sessions. Prints the new password once.
    ResetPassword(ResetPasswordArgs),
}

#[derive(Args)]
pub(crate) struct ResetPasswordArgs {
    #[arg(long)]
    pub(crate) email: String,

    /// The relay's database; the relay may keep running.
    #[arg(long, default_value = "vorp.sqlite3")]
    pub(crate) database_path: PathBuf,
}

#[derive(Args)]
pub(crate) struct TokenArgs {
    #[arg(long)]
    pub(crate) token_file: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct LoginArgs {
    /// The relay's name, such as tunnels.example.com. The token is read from
    /// a hidden prompt, or from standard input when it is not a terminal.
    pub(crate) relay_host: String,

    /// Replace a different relay_host already in the config.
    #[arg(long)]
    pub(crate) force: bool,

    /// Agent config file to write [default: ~/.config/vorp/config.yaml].
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print the config file path in use.
    Path(ConfigArgs),
    /// Print the effective settings and where each came from: flag, env,
    /// file or default. Agent flags go before `config`; relay flags after
    /// `--relay`. Secrets are never printed.
    Show(ConfigArgs),
}

#[derive(Args)]
pub(crate) struct ConfigArgs {
    /// The relay's config (`vorp serve`) instead of the agent's.
    #[arg(long)]
    pub(crate) relay: bool,

    /// Config file to read.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Relay flags (with --relay)")]
    pub(crate) serve: RelayArgs,
}

#[derive(Args)]
pub(crate) struct AgentArgs {
    /// Agent config file [default: ~/.config/vorp/config.yaml; env: VORP_CONFIG].
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Relay name; its certificate is checked against it [default: localhost].
    #[arg(long)]
    pub(crate) relay_host: Option<String>,

    /// Dial this address instead of resolving `--relay-host` on port 443.
    #[arg(long)]
    pub(crate) relay_addr: Option<SocketAddr>,

    #[arg(long)]
    pub(crate) ca_cert: Option<PathBuf>,

    #[arg(long, env = "VORP_TOKEN", hide_env_values = true)]
    pub(crate) token: Option<String>,

    #[arg(long)]
    pub(crate) token_file: Option<PathBuf>,

    #[arg(long)]
    pub(crate) upstream: Option<String>,

    #[arg(long = "subdomain")]
    pub(crate) subdomains: Vec<String>,

    #[arg(long)]
    pub(crate) allow_remote_targets: bool,
}

impl AgentArgs {
    /// The settings given as flags. The token is not a setting: it is
    /// handled separately so it never reaches the layered config.
    pub(crate) fn layer(&self) -> AgentLayer {
        AgentLayer {
            relay_host: self.relay_host.clone(),
            relay_addr: self.relay_addr,
            ca_cert: self.ca_cert.clone(),
            token_file: self.token_file.clone(),
            upstream: self.upstream.clone(),
            subdomains: (!self.subdomains.is_empty()).then(|| self.subdomains.clone()),
            // A bare switch can only turn this on; VORP_ALLOW_REMOTE_TARGETS
            // or the file can also set it to false.
            allow_remote_targets: self.allow_remote_targets.then_some(true),
        }
    }
}

#[derive(Args)]
pub(crate) struct ServeArgs {
    /// Relay config file [default: /etc/vorp/vorp.yaml; env: VORP_CONFIG].
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    #[command(flatten)]
    pub(crate) settings: RelayArgs,

    #[command(flatten)]
    pub(crate) dev: DevArgs,
}

/// `vorp serve` flags that mirror the config file. Unset flags fall back to
/// `VORP_*`, then the file, then the built-in default (`vorp config show
/// --relay` prints them).
#[derive(Args, Default)]
pub(crate) struct RelayArgs {
    /// [default: 0.0.0.0:443]
    #[arg(long)]
    pub(crate) listen: Option<SocketAddr>,

    /// Tunnels are served at `*.<base-domain>`. Required.
    #[arg(long)]
    pub(crate) base_domain: Option<String>,

    /// [default: the base domain]
    #[arg(long)]
    pub(crate) dashboard_host: Option<String>,

    /// [default: vorp.sqlite3]
    #[arg(long)]
    pub(crate) database_path: Option<PathBuf>,

    /// [default: closed]
    #[arg(long, value_enum)]
    pub(crate) signup: Option<Signup>,

    /// Concurrent TLS connections, including handshakes.
    #[arg(long)]
    pub(crate) max_connections: Option<usize>,

    /// Concurrent TLS connections from one IP (agents and browsers combined).
    #[arg(long)]
    pub(crate) max_connections_per_ip: Option<usize>,

    /// HTTP requests per second per IP; the burst is twice this.
    #[arg(long)]
    pub(crate) rate_limit_rps: Option<u64>,

    /// Concurrent proxied HTTP requests across all tunnels.
    #[arg(long)]
    pub(crate) max_requests: Option<usize>,

    /// Concurrent WebSockets across all tunnels.
    #[arg(long)]
    pub(crate) max_websockets: Option<usize>,

    /// Concurrent proxied HTTP requests per tunnel.
    #[arg(long)]
    pub(crate) tunnel_requests: Option<usize>,

    /// Concurrent WebSockets per tunnel.
    #[arg(long)]
    pub(crate) tunnel_websockets: Option<usize>,

    /// Seconds to wait for an upstream's response head (resets on upload progress).
    #[arg(long)]
    pub(crate) response_timeout_secs: Option<u64>,

    #[arg(long)]
    pub(crate) tls_cert: Option<PathBuf>,

    #[arg(long)]
    pub(crate) tls_key: Option<PathBuf>,
}

impl RelayArgs {
    pub(crate) fn layer(&self) -> RelayLayer {
        RelayLayer {
            base_domain: self.base_domain.clone(),
            dashboard_host: self.dashboard_host.clone(),
            listen: self.listen,
            database_path: self.database_path.clone(),
            signup: self.signup,
            tls: TlsLayer {
                cert: self.tls_cert.clone(),
                key: self.tls_key.clone(),
            },
            limits: LimitsLayer {
                max_connections: self.max_connections,
                max_connections_per_ip: self.max_connections_per_ip,
                rate_limit_rps: self.rate_limit_rps,
                max_requests: self.max_requests,
                max_websockets: self.max_websockets,
                tunnel_requests: self.tunnel_requests,
                tunnel_websockets: self.tunnel_websockets,
                response_timeout_secs: self.response_timeout_secs,
            },
        }
    }
}

/// Development-only flags. They have no config-file key, so a production
/// config can never turn on the loopback dev bypass by accident.
#[derive(Args)]
pub(crate) struct DevArgs {
    /// Generate a temporary certificate for local development.
    #[arg(long)]
    pub(crate) dev_self_signed: bool,

    #[arg(long, requires = "dev_self_signed")]
    pub(crate) dev_cert_out: Option<PathBuf>,

    #[arg(long, env = "VORP_DEV_TOKEN", hide_env_values = true)]
    pub(crate) dev_token: Option<String>,
}

impl From<DevArgs> for DevOptions {
    fn from(args: DevArgs) -> Self {
        Self {
            self_signed: args.dev_self_signed,
            cert_out: args.dev_cert_out,
            token: args.dev_token,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use vorp_relay::{EdgeLimits, SignupMode, TlsConfig};

    use super::*;
    use crate::config::{AgentSettings, RelaySettings};

    fn serve_args(argv: &[&str]) -> ServeArgs {
        match Cli::try_parse_from(argv)
            .expect("valid command line")
            .command
        {
            Some(Command::Serve(args)) => *args,
            _ => panic!("expected serve"),
        }
    }

    /// The flag-only `ExecStart=` from before config files existed, with no
    /// environment and no config file, yields the same relay config.
    #[test]
    fn flag_only_relay_unit_is_unchanged() {
        let args = serve_args(&[
            "vorp",
            "serve",
            "--base-domain",
            "example.com",
            "--tls-cert",
            "/etc/vorp/fullchain.pem",
            "--tls-key",
            "/etc/vorp/privkey.pem",
            "--database-path",
            "/var/lib/vorp/vorp.sqlite3",
        ]);
        assert!(args.config.is_none());
        let Ok(config) = RelaySettings::merge(
            args.settings.layer(),
            RelayLayer::default(),
            RelayLayer::default(),
        )
        .into_relay_config(args.dev.into()) else {
            panic!("flag-only config is complete");
        };
        assert_eq!(config.listen, "0.0.0.0:443".parse().expect("valid address"));
        assert_eq!(config.base_domain, "example.com");
        assert_eq!(config.dashboard_host, "example.com");
        assert_eq!(
            config.database_path,
            PathBuf::from("/var/lib/vorp/vorp.sqlite3")
        );
        assert_eq!(config.signup_mode, SignupMode::Closed);
        assert!(config.dev_token.is_none());
        let TlsConfig::Files { cert, key } = config.tls else {
            panic!("expected certificate files");
        };
        assert_eq!(cert, PathBuf::from("/etc/vorp/fullchain.pem"));
        assert_eq!(key, PathBuf::from("/etc/vorp/privkey.pem"));
        let default = EdgeLimits::default();
        assert_eq!(config.limits.max_connections, default.max_connections);
        assert_eq!(config.limits.response_timeout, default.response_timeout);
    }

    #[test]
    fn relay_flags_override_every_limit() {
        let args = serve_args(&[
            "vorp",
            "serve",
            "--base-domain",
            "example.com",
            "--dashboard-host",
            "dashboard.example.com",
            "--signup",
            "open",
            "--listen",
            "127.0.0.1:8443",
            "--max-connections",
            "1",
            "--max-connections-per-ip",
            "2",
            "--rate-limit-rps",
            "3",
            "--max-requests",
            "4",
            "--max-websockets",
            "5",
            "--tunnel-requests",
            "6",
            "--tunnel-websockets",
            "7",
            "--response-timeout-secs",
            "8",
            "--dev-self-signed",
            "--dev-cert-out",
            "/tmp/dev.pem",
        ]);
        let Ok(config) = RelaySettings::merge(
            args.settings.layer(),
            RelayLayer::default(),
            RelayLayer::default(),
        )
        .into_relay_config(args.dev.into()) else {
            panic!("config is complete");
        };
        assert_eq!(config.dashboard_host, "dashboard.example.com");
        assert_eq!(config.signup_mode, SignupMode::Open);
        let limits = config.limits;
        assert_eq!(
            (
                limits.max_connections,
                limits.max_connections_per_ip,
                limits.requests_per_second_per_ip,
                limits.max_requests,
                limits.max_websockets,
                limits.tunnel_requests,
                limits.tunnel_websockets,
                limits.response_timeout.as_secs(),
            ),
            (1, 2, 3, 4, 5, 6, 7, 8)
        );
        assert!(matches!(config.tls, TlsConfig::SelfSigned { .. }));
    }

    #[test]
    fn flag_only_agent_is_unchanged() {
        let cli = Cli::try_parse_from([
            "vorp",
            "--relay-host",
            "example.com",
            "--upstream",
            "http://127.0.0.1:3000",
            "--subdomain",
            "a",
            "--subdomain",
            "b",
            "--allow-remote-targets",
        ])
        .expect("valid command line");
        let config = AgentSettings::merge(
            cli.agent.layer(),
            AgentLayer::default(),
            AgentLayer::default(),
            None,
        )
        .into_agent_config("token".into())
        .expect("complete config");
        assert_eq!(config.relay_host, "example.com");
        assert_eq!(config.upstream, "http://127.0.0.1:3000");
        assert_eq!(
            config.requested_subdomains,
            vec![Some("a".to_owned()), Some("b".to_owned())]
        );
        assert!(config.allow_remote_targets);
    }

    #[test]
    fn agent_defaults_without_flags() {
        let cli = Cli::try_parse_from(["vorp", "--upstream", "http://127.0.0.1:3000"])
            .expect("valid command line");
        let config = AgentSettings::merge(
            cli.agent.layer(),
            AgentLayer::default(),
            AgentLayer::default(),
            None,
        )
        .into_agent_config("token".into())
        .expect("complete config");
        assert_eq!(config.relay_host, "localhost");
        assert_eq!(config.requested_subdomains, vec![None]);
        assert!(!config.allow_remote_targets);
    }
}
