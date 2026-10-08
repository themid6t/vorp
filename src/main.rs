mod cli;
mod config;
mod login;
mod token;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{ArgMatches, CommandFactory, FromArgMatches, parser::ValueSource};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use cli::{
    AdminCommand, Cli, Command, ConfigCommand, LoginArgs, ResetPasswordArgs, ServeArgs, ShowArgs,
    TokenArgs,
};
use config::{AgentLayer, Merge, RelayLayer, Row, Source};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    // The matches of the subcommand, for the sources of its flags.
    let sub = matches.subcommand().map_or(&matches, |(_, sub)| sub);
    match cli.command {
        Some(Command::Serve(args)) => serve(*args, cli.config, sub).await,
        Some(Command::Authtoken(args)) => save_token(args),
        Some(Command::Login(args)) => run_login(args, cli.config, cli.agent.token_file),
        Some(Command::Config(ConfigCommand::Path { relay })) => {
            print_config_path(relay, cli.config)
        }
        Some(Command::Config(ConfigCommand::Show(args))) => {
            let show = sub.subcommand().map_or(sub, |(_, show)| show);
            show_config(*args, cli.config, cli.agent, &matches, show)
        }
        Some(Command::Admin(AdminCommand::ResetPassword(args))) => reset_password(args).await,
        None => run_agent(cli, &matches).await,
    }
}

/// `Flag` or `Env` when the user set clap id `id`; `None` for a built-in
/// default or nothing.
fn explicit(matches: &ArgMatches) -> impl Fn(&str) -> Option<Source> + '_ {
    |id| match matches.value_source(id) {
        Some(ValueSource::CommandLine) => Some(Source::Flag),
        Some(ValueSource::EnvVariable) => Some(Source::Env),
        _ => None,
    }
}

/// Whether `vorp config show` got a relay flag on its command line.
fn explicit_flags(show: &ArgMatches) -> bool {
    show.ids().any(|id| {
        !["relay", "config"].contains(&id.as_str())
            && show.value_source(id.as_str()) == Some(ValueSource::CommandLine)
    })
}

/// The `config` row: the file, whether it exists, and who chose it.
fn config_row(path: Option<&PathBuf>, named: bool, matches: &ArgMatches) -> Row {
    let source = if named {
        explicit(matches)("config").unwrap_or(Source::Flag)
    } else {
        Source::Default
    };
    let row = path.map(|path| {
        let missing = if path.exists() { "" } else { " (not found)" };
        (format!("{}{missing}", path.display()), source)
    });
    ("config", row)
}

fn agent_config_path(config: Option<PathBuf>) -> Option<PathBuf> {
    config.or_else(|| {
        config::agent_dir(|name| std::env::var_os(name)).map(|dir| dir.join("config.yaml"))
    })
}

/// The agent's settings merged from flags, environment, file and defaults,
/// with a `vorp config show` row for each, the config file's first. The
/// default token file is `authtoken` next to the config file.
fn agent_settings(
    config: Option<PathBuf>,
    mut clap: AgentLayer,
    matches: &ArgMatches,
) -> Result<(AgentLayer, Vec<Row>)> {
    let named = config.is_some();
    let path = agent_config_path(config);
    let file = match &path {
        Some(path) => config::read(path, named, true)?,
        None => AgentLayer::default(),
    };
    if clap.token_file.is_none() {
        clap.token_file = path.as_ref().map(|path| path.with_file_name("authtoken"));
    }
    let explicit = explicit(matches);
    let mut merge = Merge::new(&explicit);
    merge.rows.push(config_row(path.as_ref(), named, matches));
    Ok((clap.merge(file, &mut merge), merge.rows))
}

/// Like [`agent_settings`], for `vorp serve`.
fn relay_settings(
    config: Option<PathBuf>,
    clap: RelayLayer,
    matches: &ArgMatches,
) -> Result<(RelayLayer, Vec<Row>)> {
    let named = config.is_some();
    let path = config.unwrap_or_else(|| PathBuf::from(config::RELAY_CONFIG));
    let file = config::read(&path, named, false)?;
    let explicit = explicit(matches);
    let mut merge = Merge::new(&explicit);
    merge.rows.push(config_row(Some(&path), named, matches));
    Ok((clap.merge(file, &mut merge), merge.rows))
}

/// Prints each effective setting and its source. Secrets are never printed.
fn show_config(
    args: ShowArgs,
    config: Option<PathBuf>,
    agent: AgentLayer,
    matches: &ArgMatches,
    show: &ArgMatches,
) -> Result<()> {
    if !args.relay && explicit_flags(show) {
        bail!("relay flags need --relay; agent flags go before `config`");
    }
    let rows = if args.relay {
        relay_settings(config, args.serve, show)?.1
    } else {
        agent_rows(config, agent, matches)?
    };
    print_rows(&rows)
}

/// The agent rows plus where the token comes from, never its value.
fn agent_rows(config: Option<PathBuf>, clap: AgentLayer, matches: &ArgMatches) -> Result<Vec<Row>> {
    let (_, mut rows) = agent_settings(config, clap, matches)?;
    let token_row = match explicit(matches)("token") {
        Some(Source::Flag) => Some(("set by --token".to_owned(), Source::Flag)),
        Some(source) => Some(("set by VORP_TOKEN".to_owned(), source)),
        _ => rows
            .iter()
            .find(|(key, _)| *key == "token_file")
            .and_then(|(_, file)| file.clone())
            .map(|(path, source)| (format!("read from {path}"), source)),
    };
    rows.push(("token", token_row));
    Ok(rows)
}

fn print_rows(rows: &[Row]) -> Result<()> {
    use std::io::Write;

    let width = rows.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    let lines: Vec<String> = rows
        .iter()
        .map(|(key, value)| match value {
            Some((value, source)) => format!("{key:<width$}  {value}  ({source})"),
            None => format!("{key:<width$}  (not set)"),
        })
        .collect();
    writeln!(std::io::stdout().lock(), "{}", lines.join("\n")).context("print settings")
}

fn print_config_path(relay: bool, config: Option<PathBuf>) -> Result<()> {
    use std::io::Write;

    let path = if relay {
        config.unwrap_or_else(|| PathBuf::from(config::RELAY_CONFIG))
    } else {
        agent_config_path(config).context("no config directory; pass --config")?
    };
    writeln!(std::io::stdout().lock(), "{}", path.display()).context("print config path")
}

async fn serve(args: ServeArgs, config: Option<PathBuf>, matches: &ArgMatches) -> Result<()> {
    let (settings, _) = relay_settings(config, args.settings, matches)?;
    let config = settings
        .into_relay_config(args.dev)
        .context("relay configuration is incomplete")?;
    vorp_relay::serve_until(config, shutdown_on_signal()?)
        .await
        .context("relay stopped")
}

async fn run_agent(cli: Cli, matches: &ArgMatches) -> Result<()> {
    let (settings, _) = agent_settings(cli.config, cli.agent, matches)?;
    let token = match cli.token {
        Some(token) => token,
        None => {
            let path = settings
                .token_file
                .clone()
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

fn run_login(args: LoginArgs, config: Option<PathBuf>, token_file: Option<PathBuf>) -> Result<()> {
    let path = agent_config_path(config).context("no config directory; pass --config")?;
    login::login(&path, token_file, &args.relay_host, args.force, || {
        token::read_from_stdin(true)
    })
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use vorp_relay::{EdgeLimits, SignupMode, TlsConfig};

    use super::*;
    use crate::config::TempDir;

    fn parse(argv: &[&str]) -> (Cli, ArgMatches) {
        let matches = Cli::command()
            .try_get_matches_from(argv)
            .expect("valid command line");
        let cli = Cli::from_arg_matches(&matches).expect("matches fit Cli");
        (cli, matches)
    }

    fn relay(argv: &[&str]) -> (vorp_relay::RelayConfig, Vec<Row>) {
        let (cli, matches) = parse(argv);
        let Some(Command::Serve(args)) = cli.command else {
            panic!("expected serve");
        };
        let serve = matches.subcommand_matches("serve").expect("serve matches");
        let (settings, rows) = relay_settings(cli.config, args.settings, serve).expect("settings");
        let config = settings
            .into_relay_config(args.dev)
            .expect("complete config");
        (config, rows)
    }

    fn source<'a>(rows: &'a [Row], key: &str) -> Option<&'a (String, Source)> {
        rows.iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, value)| value.as_ref())
    }

    fn write(dir: &Path, name: &str, text: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write config");
        path.display().to_string()
    }

    /// The flag-only `ExecStart=` from before config files existed, with an
    /// empty config file, yields the same relay config.
    #[test]
    fn flag_only_relay_unit_is_unchanged() {
        let dir = TempDir::new("main-flag-only-relay");
        let empty = write(&dir.0, "empty.yaml", "");
        let (config, _) = relay(&[
            "vorp",
            "serve",
            "--config",
            &empty,
            "--base-domain",
            "example.com",
            "--tls-cert",
            "/etc/vorp/fullchain.pem",
            "--tls-key",
            "/etc/vorp/privkey.pem",
            "--database-path",
            "/var/lib/vorp/vorp.sqlite3",
        ]);
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
        assert_eq!(
            format!("{:?}", config.limits),
            format!("{:?}", EdgeLimits::default())
        );
    }

    /// Every relay key from the file, one overridden by a flag; also checks
    /// that each `vorp config show` key is a real clap id.
    #[test]
    fn relay_file_under_flags() {
        let dir = TempDir::new("main-relay-file");
        let file = write(
            &dir.0,
            "vorp.yaml",
            "base_domain: example.com\ndashboard_host: dash.example.com\n\
             listen: 127.0.0.1:1\ndatabase_path: /db\nsignup: open\n\
             tls: {cert: /c.pem, key: /k.pem}\nlimits: {max_connections: 5, max_requests: 6}\n",
        );
        let (config, rows) = relay(&["vorp", "serve", "--config", &file, "--max-requests", "7"]);
        assert_eq!(config.dashboard_host, "dash.example.com");
        assert_eq!(config.signup_mode, SignupMode::Open);
        assert_eq!(config.limits.max_connections, 5);
        assert_eq!(config.limits.max_requests, 7);
        assert_eq!(
            source(&rows, "limits.max_connections"),
            Some(&("5".to_owned(), Source::File))
        );
        assert_eq!(
            source(&rows, "limits.max_requests"),
            Some(&("7".to_owned(), Source::Flag))
        );
        assert_eq!(
            source(&rows, "limits.max_websockets").map(|(_, s)| *s),
            Some(Source::Default)
        );
        assert_eq!(source(&rows, "config"), Some(&(file, Source::Flag)));
    }

    #[test]
    fn flag_only_agent_is_unchanged() {
        let dir = TempDir::new("main-flag-only-agent");
        let empty = write(&dir.0, "empty.yaml", "");
        let agent = |argv: &[&str]| {
            let (cli, matches) = parse(argv);
            let (settings, _) =
                agent_settings(Some(empty.clone().into()), cli.agent, &matches).expect("settings");
            settings
                .into_agent_config("token".into())
                .expect("complete config")
        };
        let config = agent(&[
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
        ]);
        assert_eq!(config.relay_host, "example.com");
        assert_eq!(config.upstream, "http://127.0.0.1:3000");
        assert_eq!(
            config.requested_subdomains,
            vec![Some("a".to_owned()), Some("b".to_owned())]
        );
        assert!(config.allow_remote_targets);

        let config = agent(&["vorp", "--upstream", "http://127.0.0.1:3000"]);
        assert_eq!(config.relay_host, "localhost");
        assert_eq!(config.requested_subdomains, vec![None]);
        assert!(!config.allow_remote_targets);
    }

    #[test]
    fn agent_token_row_never_shows_the_token() {
        let dir = TempDir::new("main-token-row");
        let file = write(&dir.0, "config.yaml", "relay_host: example.com\n");
        let (cli, matches) = parse(&["vorp", "--token", "vorp_1_secret", "config", "show"]);
        let rows = agent_rows(Some(file.into()), cli.agent, &matches).expect("rows");
        assert_eq!(
            source(&rows, "token"),
            Some(&("set by --token".to_owned(), Source::Flag))
        );
        assert_eq!(
            source(&rows, "relay_host"),
            Some(&("example.com".to_owned(), Source::File))
        );
        assert!(!format!("{rows:?}").contains("vorp_1_secret"));
    }

    #[test]
    fn relay_flags_on_config_show_need_relay() {
        for (argv, expected) in [
            (
                &["vorp", "config", "show", "--listen", "127.0.0.1:1"][..],
                true,
            ),
            (
                &["vorp", "config", "show", "--config", "/c.yaml"][..],
                false,
            ),
            (
                &["vorp", "--upstream", "http://127.0.0.1:1", "config", "show"][..],
                false,
            ),
        ] {
            let (_, matches) = parse(argv);
            let show = matches
                .subcommand_matches("config")
                .and_then(|config| config.subcommand_matches("show"))
                .expect("show matches");
            assert_eq!(explicit_flags(show), expected, "{argv:?}");
        }
    }
}
