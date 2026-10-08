//! The built binary, for what needs real environment variables: clap reads
//! them from the process, so setting them inside the unit tests would race.

use std::process::{Command, Output};

/// A directory under the system temp dir, removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vorp-cli-{name}-{}", std::process::id()));
        // A leftover from an earlier, aborted run; absence is the normal case.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; a leftover temp dir is harmless.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `vorp` with `VORP_RELAY_ADDR=garbage`, no other `VORP_*` variable,
/// and an empty config directory.
fn vorp(dir: &TempDir, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vorp"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("VORP_") {
            command.env_remove(name);
        }
    }
    command
        .args(args)
        .env("VORP_RELAY_ADDR", "garbage")
        .env("XDG_CONFIG_HOME", &dir.0)
        .env("APPDATA", &dir.0)
        .env("NO_COLOR", "1")
        .output()
        .expect("run vorp")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A malformed agent variable fails only the commands that use agent
/// settings: the agent run and the agent's `vorp config show`.
#[test]
fn malformed_agent_env_fails_only_agent_commands() {
    let dir = TempDir::new("agent-env");
    let missing = dir.0.join("absent.yaml").display().to_string();
    let database = dir.0.join("absent.sqlite3").display().to_string();
    // (argv, parsing must fail on VORP_RELAY_ADDR)
    let cases: &[(&[&str], bool)] = &[
        (&["serve", "--help"], false),
        (&["serve", "--config", &missing], false),
        (&["config", "show", "--relay", "--config", &missing], false),
        (&["config", "path"], false),
        (
            &[
                "admin",
                "reset-password",
                "--email",
                "a@b.c",
                "--database-path",
                &database,
            ],
            false,
        ),
        (&["authtoken", "--token-file", &missing], false),
        (&["login", "bad host"], false),
        (&["--upstream", "http://127.0.0.1:1"], true),
        (&["config", "show"], true),
    ];
    for (args, fails) in cases {
        let output = vorp(&dir, args);
        let shown = text(&output);
        assert_eq!(shown.contains("'garbage'"), *fails, "{args:?}: {shown}");
    }
}

/// Both modes log the config file they read, and never its values.
#[test]
fn config_file_loaded_is_logged() {
    let dir = TempDir::new("loaded");
    let relay = dir.0.join("vorp.yaml");
    std::fs::write(&relay, "base_domain: secret-value.example\n").expect("write");
    let agent = dir.0.join("config.yaml");
    std::fs::write(&agent, "relay_host: secret-value.example\n").expect("write");
    for args in [
        vec!["serve", "--config", relay.to_str().expect("utf-8 path")],
        vec![
            "--config",
            agent.to_str().expect("utf-8 path"),
            "--relay-addr",
            "127.0.0.1:1",
        ],
    ] {
        let shown = text(&vorp(&dir, &args));
        assert!(shown.contains("config file loaded"), "{args:?}: {shown}");
        assert!(!shown.contains("secret-value"), "{args:?}: {shown}");
    }
    // No file read, no line: the agent's default file is absent.
    let shown = text(&vorp(&dir, &["--relay-addr", "127.0.0.1:1"]));
    assert!(!shown.contains("config file loaded"), "{shown}");
}
