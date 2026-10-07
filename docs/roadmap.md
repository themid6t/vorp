# vorp roadmap

## Where it stands (v0.0.1)

vorp has run in production since 2026-10-07. A release has:

- A TLS relay on one `:443` listener that routes on ALPN: agent sessions over
  yamux, and the dashboard plus public tunnels over HTTP/1.1 and HTTP/2.
- An outbound-only agent. Request and response bodies stream in both
  directions with backpressure; SSE and WebSocket work through a tunnel.
- SQLite state: users with argon2id passwords, opaque server-side sessions,
  agent tokens stored as SHA-256 hashes, bind policies, reserved names with
  admin approval, and live revocation that closes the token's sessions.
- Per-user quotas (tunnels, concurrent requests, bandwidth) that slow traffic
  before refusing it, plus edge limits on connections, request rate, slow
  headers and WebSockets.
- An embedded Svelte dashboard, with a per-user traffic feed.
- Supplied certificate files, re-read every 30 seconds without dropping
  connections.
- Signed static Linux amd64 and arm64 builds with an installer
  ([releases.md](releases.md)).

Not yet: built-in ACME, a setup command, service installation, macOS builds,
and a container image. The relay still gets its certificate from Certbot
([certificates.md](certificates.md)).

## Crate layout

```
vorp/
├── AGENTS.md               ground rules (CLAUDE.md is a symlink to it)
├── docs/                   protocol, API, schema, releases, certificates, this file
├── crates/
│   ├── protocol/           frames, message types, codec. No I/O except AsyncRead/Write.
│   ├── store/              SQLite: migrations, models, every SQL statement.
│   ├── relay/              serve mode: ALPN listener, sessions, tunnel map, proxy.
│   ├── agent/              agent mode: dial, register, forward to upstream.
│   └── web/                axum: dashboard, /api, auth; the Svelte UI in web/ui.
├── deploy/                 staging unit, deploy and certificate hook scripts
├── install.sh              verified installer, published with each release
└── src/main.rs             the single binary; wires the modes together. Thin.
```

ACME will be a module inside `relay` (`crates/relay/src/acme/`), not a crate.

## v0.1: open-source release

Goal: someone (or an AI agent acting for them) goes from nothing to a working
relay with one install command and one setup command, and from a fresh laptop
to a live tunnel with two commands. Decided 2026-10-05: **MIT license**,
**built-in ACME is in v0.1**, and **native services are the default** for
keeping things running, with pm2 and containers as cookbooks.

Order matters: ACME comes first, because `setup` and `doctor` depend on it.

1. **Project basics.** `LICENSE` (MIT) is in place. Still to add:
   `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`.
2. **Built-in ACME DNS-01** (`instant-acme`): wildcard issuance for `domain` +
   `*.domain`, a renewal task, hot replacement through the existing
   certificate-reload path, and an `AcmeDns` trait with Cloudflare first.
   Supplied certificate files remain an alternative. The certificate has to be
   a wildcard because tunnel names are random; per-name TLS-ALPN-01
   certificates would hit Let's Encrypt's limit of 50 per week.
3. **Settings from the environment.** Every `serve` flag also reads a
   `VORP_*` variable (clap `env`), so systemd, Docker and agents share one
   `/etc/vorp/vorp.env`. No new config-file format.
4. **`vorp doctor [--json]`.** Checks DNS for the root and wildcard, that :443
   is free or held by vorp, the DNS-provider token, certificate validity, and
   database access. Output is machine-readable, with stable check ids so an
   agent can act on failures.
5. **`vorp setup`.** Runs non-interactively with flags (`--domain`,
   `--cloudflare-token-file`, `--admin-email`, `--yes`). It runs `doctor`,
   creates missing DNS records, gets the certificate, writes
   `/etc/vorp/vorp.env`, installs and starts the service, creates the admin,
   and prints the dashboard URL.
6. **Close the bootstrap race.** Today the first visitor to the dashboard
   becomes admin. On a public install the admin is created from the command
   line by `setup`, or the relay prints a one-time setup code that the
   bootstrap page requires.
7. **`vorp service install`** for the relay (a system unit, hardened like the
   staging unit) and the agent (a systemd user unit with linger on Linux,
   launchd on macOS).
8. **Agent first run.** `vorp login <relay>` stores the relay host and the
   token once; the token comes from a prompt or standard input, never an
   argument. `vorp http <port> [--name <reserved>]` opens a tunnel to
   `127.0.0.1:<port>`. The existing flags keep working.
9. **"Connect an agent" in the dashboard.** After a token is created, the
   dashboard shows the install command, `vorp login`, `vorp http` and a
   Compose sidecar snippet, filled in with this relay's domain and version so
   the agent always matches the relay's protocol.
10. **Releases for every platform.** Add macOS amd64 and arm64 to the existing
    Linux builds, and teach `install.sh` to install them.
11. **Docs.** A README quickstart and `docs/setup.md`, written as a runbook
    with exact commands, the expected output, and the fix for each `doctor`
    failure, for people and AI agents alike. Cookbooks in `docs/cookbook/` for
    systemd, launchd, pm2 (token file only: a `--token` argument shows in `ps`
    and in pm2's saved process list), and a Docker Compose sidecar
    (`--allow-remote-targets --upstream http://app:3000`). User-facing docs must
    say that a tunnel URL is not access control.

## v0.2

- Several named tunnels from one agent process (`tunnels.toml`, `vorp start
  [name]`). Today one agent process serves one upstream.
- A container image and Compose file for the relay; a Homebrew tap.
- CLI login approved in the dashboard instead of copying a token.
- Kubernetes: outbound-only agent sidecar probes and a Helm chart.
- API tokens and an agent-facing API description. The flow is still to be
  designed.
