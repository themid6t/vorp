mod cli;
mod config;
mod login;
mod token;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, parser::ValueSource};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use cli::{
    AdminCommand, AgentArgs, Cli, Command, ConfigArgs, ConfigCommand, LoginArgs, RelayArgs,
    ResetPasswordArgs, ServeArgs, TokenArgs,
};
use config::{AgentLayer, AgentSettings, Env, RelayLayer, RelaySettings, Setting, Source};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let matches = Cli::command().get_matches();
    // Whether --token or VORP_TOKEN supplied the token, for `vorp config show`.
    let token_source = match matches.value_source("token") {
        Some(ValueSource::CommandLine) => Some(Source::Flag),
        Some(ValueSource::EnvVariable) => Some(Source::Env),
        _ => None,
    };
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    match cli.command {
        Some(Command::Serve(args)) => serve(*args).await,
        Some(Command::Authtoken(args)) => save_token(args),
        Some(Command::Login(args)) => run_login(args),
        Some(Command::Config(command)) => run_config(*command, &cli.agent, token_source),
        Some(Command::Admin(AdminCommand::ResetPassword(args))) => reset_password(args).await,
        None => run_agent(cli.agent).await,
    }
}

/// The relay's config path and its settings merged from flags, environment,
/// file and defaults.
fn relay_settings(
    flags: &RelayArgs,
    config_flag: Option<PathBuf>,
    env: &Env,
) -> Result<(Setting<PathBuf>, RelaySettings)> {
    let path = config::relay_config_path(config_flag, env)?;
    let file = config::read_layer(Some(&path), RelayLayer::parse)?;
    let settings = RelaySettings::merge(flags.layer(), RelayLayer::from_env(env)?, file);
    Ok((path, settings))
}

/// The agent's config path (if there is a config directory) and its merged
/// settings. The default token file is `authtoken` next to the config file.
fn agent_settings(
    flags: &AgentArgs,
    config_flag: Option<PathBuf>,
    env: &Env,
) -> Result<(Option<Setting<PathBuf>>, AgentSettings)> {
    let path = config::agent_config_path(config_flag, env)?;
    let file = config::read_layer(path.as_ref(), AgentLayer::parse)?;
    let default_token = path.as_ref().map(|p| p.value.with_file_name("authtoken"));
    let settings = AgentSettings::merge(
        flags.layer(),
        AgentLayer::from_env(env)?,
        file,
        default_token,
    );
    Ok((path, settings))
}

async fn serve(args: ServeArgs) -> Result<()> {
    let env = Env::from_process()?;
    let (_, settings) = relay_settings(&args.settings, args.config, &env)?;
    let config = settings
        .into_relay_config(args.dev.into())
        .context("relay configuration is incomplete")?;
    vorp_relay::serve_until(config, shutdown_on_signal()?)
        .await
        .context("relay stopped")
}

async fn run_agent(args: AgentArgs) -> Result<()> {
    let env = Env::from_process()?;
    let (_, settings) = agent_settings(&args, args.config.clone(), &env)?;
    let token = match args.token {
        Some(token) => token,
        None => {
            let path = settings
                .token_file
                .as_ref()
                .map(|setting| setting.value.clone())
                .context("no config directory; pass --token-file or set VORP_TOKEN")?;
            std::fs::read_to_string(&path)
                .with_context(|| format!("read agent token from {}", path.display()))?
                .trim()
                .to_owned()
        }
    };
    if token.is_empty() {
        bail!("agent token is empty");
    }
    let config = settings
        .into_agent_config(token)
        .context("agent configuration is incomplete")?;
    config
        .validate()
        .context("agent configuration is invalid")?;
    vorp_agent::run_until(config, shutdown_on_signal()?)
        .await
        .context("agent stopped")
}

fn save_token(args: TokenArgs) -> Result<()> {
    let path = match args.token_file {
        Some(path) => path,
        None => token::default_path()?,
    };
    let raw = token::read_from_stdin(false)?;
    token::save(&path, token::validate(&raw)?)
}

fn run_login(args: LoginArgs) -> Result<()> {
    let env = Env::from_process()?;
    let path = config::agent_config_path(args.config, &env)?
        .context("no config directory; pass --config")?;
    login::login(&path, &env, &args.relay_host, args.force, || {
        token::read_from_stdin(true)
    })
}

fn run_config(
    command: ConfigCommand,
    agent: &AgentArgs,
    token_source: Option<Source>,
) -> Result<()> {
    let (show, args): (bool, ConfigArgs) = match command {
        ConfigCommand::Path(args) => (false, args),
        ConfigCommand::Show(args) => (true, args),
    };
    if !args.relay && args.serve.layer() != RelayLayer::default() {
        bail!("relay flags need --relay; agent flags go before `config`");
    }
    let env = Env::from_process()?;
    let config_flag = args.config.or_else(|| agent.config.clone());
    if show {
        show_config(
            args.relay,
            &args.serve,
            config_flag,
            agent,
            token_source,
            &env,
        )
    } else {
        print_config_path(args.relay, config_flag, &env)
    }
}

fn print_config_path(relay: bool, config_flag: Option<PathBuf>, env: &Env) -> Result<()> {
    use std::io::Write;

    let path = if relay {
        Some(config::relay_config_path(config_flag, env)?)
    } else {
        config::agent_config_path(config_flag, env)?
    };
    let path = path.context("no config directory; pass --config")?;
    writeln!(std::io::stdout().lock(), "{}", path.value.display()).context("print config path")
}

/// Prints each effective setting and its source. Secrets are never printed:
/// the token row says only where the token comes from.
fn show_config(
    relay: bool,
    relay_flags: &RelayArgs,
    config_flag: Option<PathBuf>,
    agent: &AgentArgs,
    token_source: Option<Source>,
    env: &Env,
) -> Result<()> {
    let (path, mut rows) = if relay {
        let (path, settings) = relay_settings(relay_flags, config_flag, env)?;
        (Some(path), settings.rows())
    } else {
        let (path, settings) = agent_settings(agent, config_flag, env)?;
        let mut rows = settings.rows();
        rows.push(token_row(token_source, settings.token_file.as_ref()));
        (path, rows)
    };
    let file_row = path.map(|path| {
        let missing = if path.value.exists() {
            ""
        } else {
            " (not found)"
        };
        (format!("{}{missing}", path.value.display()), path.source)
    });
    rows.insert(0, ("config", file_row));
    config::write_rows(&mut std::io::stdout().lock(), &rows).context("print settings")
}

/// Where the token comes from, never its value.
fn token_row(source: Option<Source>, token_file: Option<&Setting<PathBuf>>) -> config::Row {
    let value = match (source, token_file) {
        (Some(Source::Flag), _) => Some(("set by --token".to_owned(), Source::Flag)),
        (Some(source), _) => Some(("set by VORP_TOKEN".to_owned(), source)),
        (None, Some(file)) => Some((format!("read from {}", file.value.display()), file.source)),
        (None, None) => None,
    };
    ("token", value)
}

async fn reset_password(args: ResetPasswordArgs) -> Result<()> {
    use std::io::Write;
    // Opening creates a missing file; refuse instead, since a typo in the
    // path would otherwise report "no account" against a fresh empty database.
    if !args.database_path.exists() {
        bail!("database {} does not exist", args.database_path.display());
    }
    let repository = vorp_store::Repository::open(&args.database_path)
        .await
        .with_context(|| format!("open database {}", args.database_path.display()))?;
    let password = vorp_web::reset_password(&repository, &args.email)
        .await
        .context("reset password")?;
    // The password is the command's output, so it goes to stdout, never to
    // the tracing log.
    writeln!(
        std::io::stdout().lock(),
        "New password for {}: {password}\nAll of its sessions were ended. The dashboard asks for a new password at the next login.",
        args.email.trim()
    )
    .context("print new password")?;
    Ok(())
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
