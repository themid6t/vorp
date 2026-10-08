//! Command-line definitions. The settings themselves live in `config`, where
//! one struct per mode serves as both the flags and the config file.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::config::{AgentLayer, DevArgs, RelayLayer};

#[derive(Parser)]
#[command(name = "vorp", version, about = "Self-hosted reverse tunnels")]
pub(crate) struct Cli {
    /// Config file [default: ~/.config/vorp/config.yaml, or /etc/vorp/vorp.yaml
    /// for the relay].
    #[arg(long, global = true, env = "VORP_CONFIG")]
    pub(crate) config: Option<PathBuf>,

    #[arg(long, env = "VORP_TOKEN", hide_env_values = true)]
    pub(crate) token: Option<String>,

    #[command(flatten)]
    pub(crate) agent: AgentLayer,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
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
    Config(ConfigCommand),
    /// Account recovery, run on the relay host against its database.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Args)]
pub(crate) struct ServeArgs {
    #[command(flatten)]
    pub(crate) settings: RelayLayer,

    #[command(flatten)]
    pub(crate) dev: DevArgs,
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
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print the config file path in use.
    Path {
        /// The relay's config (`vorp serve`) instead of the agent's.
        #[arg(long)]
        relay: bool,
    },
    /// Print the effective settings and where each came from: flag, env,
    /// file or default. Agent flags go before `config`; relay flags after
    /// `--relay`. Secrets are never printed.
    Show(Box<ShowArgs>),
}

#[derive(Args)]
pub(crate) struct ShowArgs {
    /// The relay's settings (`vorp serve`) instead of the agent's.
    #[arg(long)]
    pub(crate) relay: bool,

    #[command(flatten, next_help_heading = "Relay flags (with --relay)")]
    pub(crate) serve: RelayLayer,
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
