# vorp build roadmap

How this gets built, split so several agents can work at once without colliding.

Current state: M0, M1, and M2 are implemented; M3 is partial; M4 has a working
staging build/deployment pipeline but other distribution work remains. Local
smoke checks cover a streamed HTTP upload and response, a WebSocket echo,
SQLite-backed agent authentication, live token revocation,
and certificate-file hot reload. Production TLS currently requires supplied
certificate files; the development mode generates a self-signed certificate
that the agent can explicitly trust. Staging obtains its wildcard certificate
externally with Certbot and Cloudflare DNS-01, as described in
[certificates.md](certificates.md). Built-in ACME issuance is not implemented.

| Milestone | Status | Delivered / remaining |
|---|---|---|
| M0 — scaffold | Done | Five-crate workspace, pinned toolchain, CI gates, protocol and schema contracts. |
| M1 — tunnels | Done | TLS/ALPN relay, outbound agent over yamux, streamed HTTP bodies, WebSocket forwarding, tunnel lifecycle. |
| M2 — auth and state | Done | SQLite repository, local users and opaque sessions, dashboard/API (Svelte app embedded in the binary since 2026-10-05), bind ACL, scoped traffic feed, live token revocation. |
| M3 — production surface | Partial | Supplied certificate files reload without dropping existing connections. ACME DNS-01 and renewal, agent sidecar probes, `install-service`, and `doctor` remain. |
| M4 — distribution | Partial | GitHub Actions builds a static Linux amd64 binary and deploys passing `main` commits to staging. Container image, Compose, Helm, production release workflow, signed manifest, and installer remain. |

Two rules make the parallelism actually work:

1. **Every workstream owns a disjoint set of paths.** No two agents edit the same
   file. Where a boundary is shared, one workstream owns it and the others
   consume it.
2. **Seams are agreed before the work starts, not during.** M0 lands the types,
   signatures and schema as compiling stubs. After that, a workstream can be
   written against a `todo!()` it does not own.

---

## Crate layout

```
vorp/
├── Cargo.toml              workspace
├── rust-toolchain.toml     pinned toolchain
├── AGENTS.md               ground rules (CLAUDE.md is a symlink to it)
├── docs/
│   ├── protocol.md         the wire contract
│   ├── roadmap.md          this file
│   └── schema.md           SQLite schema + repository signatures
├── crates/
│   ├── protocol/           frames, message types, codec. No I/O except AsyncRead/Write.
│   ├── store/              SQLite: migrations, models, every SQL statement.
│   ├── relay/              serve mode: ALPN listener, sessions, tunnel map, proxy, ACME.
│   ├── agent/              agent mode: dial, register, forward to upstream.
│   └── web/                axum: dashboard, /api, auth, embedded assets.
└── src/main.rs             the single binary; wires modes together. Thin.
```

`acme` is a module inside `relay`, not a crate — it is a few hundred lines and
has exactly one consumer.

---

## Workstreams

| # | Workstream | Owns | Depends on |
|---|---|---|---|
| **1** | Wire protocol + agent | `crates/protocol`, `crates/agent`, `docs/protocol.md` | — |
| **2** | Dashboard, auth, persistence | `crates/web`, `crates/store`, `docs/schema.md` | M0 seams |
| **3** | Relay / tunnel control plane | `crates/relay` | `protocol` types, `store` signatures |
| **4** | Integrations | `crates/relay/src/acme/`, `crates/agent/src/health.rs`, `src/main.rs` subcommands | M1 working tunnel |
| **5** | Deployment | `deploy/`, `Dockerfile`, `.github/workflows/`, `install.sh` | M1 buildable binary |

### 1 — Wire protocol + agent

The protocol crate and the agent are one workstream because they are two halves
of the same contract, and the agent is the protocol's first real consumer and its
best test.

- `protocol`: frame codec (`encode`/`decode` over `AsyncRead`/`AsyncWrite`), the
  message enum, payload types, error codes, close reasons, `PROTOCOL_VERSION`.
  Table-driven tests are **required**: round-trip, truncated frame, oversized
  declared length, unknown type, chunk/end sequencing.
- `agent`: TLS dial with ALPN verification, register, heartbeat sender, tunnel
  registration + recoverable re-register with backoff, request-stream handler
  that streams to and from the configured upstream, upstream validation (fail
  closed, loopback-only unless `--allow-remote-targets`), persisted authtoken.

Done when: the agent can register against a stub relay and stream a request both
ways, with the body never collected.

### 2 — Dashboard, auth, persistence

Owns the schema outright, because auth is its core and split ownership of
migrations is how two agents corrupt each other's work.

- `store`: migrations, `users`, `sessions`, `tokens`, `reserved_subdomains`,
  `profiles`-equivalent fields; every SQL statement in this crate and nowhere
  else.
- `web`: argon2id registration/login, opaque server-side sessions
  (`HttpOnly; Secure; SameSite=Lax`), first-run bootstrap admin, admin-creates-user,
  `signup: open | closed`, token CRUD (raw value shown once), reserved
  subdomain CRUD, tunnel list/force-close, per-user traffic feed, embedded assets.

**Deliver the token and subdomain repository methods first**, ahead of the
dashboard UI — workstream 3 is blocked on them and on nothing else here.

Done when: a user can register, log in, mint a token, see their tunnels, and
revoke a token — and revoking kills the live session.

### 3 — Relay / tunnel control plane

- Single `:443` rustls acceptor, ALPN dispatch, `Host`-based split between
  dashboard and tunnel proxy.
- Agent session lifecycle: register, token resolution, `(user_id, machine_id)`
  slot displacement, heartbeat reader, teardown-once, `Arc::ptr_eq`-guarded map
  removal.
- Tunnel map, auto-slug assignment (CSPRNG, no fallback), atomic check-and-set.
- `decide_bind` — the bind ACL as a pure function with a table-driven test,
  ported from the Go `decideBind`.
- The proxy path: normalization (§8 of the protocol spec), streaming both
  directions, WebSocket upgrade, concurrency caps, idle/head timeouts.
- `disconnect_token(token_id)` so revocation terminates live sessions.

Done when: a browser request reaches a local service through the relay and
streams back, and the bind ACL is enforced.

### 4 — Integrations

- ACME DNS-01 via `instant-acme`: wildcard issuance, renewal task, hot cert
  reload into rustls without dropping connections.
- An `AcmeDns` trait with Cloudflare as the first implementation.
- Bring-your-own-cert path: if `tls.cert`/`tls.key` are configured, skip ACME and
  watch the files for changes.
- Agent sidecar liveness/readiness probes. Keep the agent outbound-only; use
  exec probes or local state instead of adding an inbound HTTP listener.
- `vorp install-service` (emit a systemd unit) and `vorp doctor` (diagnose DNS,
  cert, port binding, and connectivity in machine-readable output, so an agent
  setting this up can act on the result).

### 5 — Deployment

- Multi-arch distroless container image, non-root, read-only rootfs compatible.
- `docker-compose.yml` for the relay, one-command local bring-up.
- Helm chart: relay Deployment/Service/Ingress-free (it owns :443), plus an agent
  sidecar example.
- Release workflow: cross-compiled binaries, checksums, signed manifest.
- `install.sh` honouring the signed manifest.

Mostly not Rust, so it runs in parallel with everything from M1 onward.

---

## Milestones

### M0 — Scaffold *(done)*

Workspace, five crates, `rust-toolchain.toml`, CI with all four gates
(`fmt`, `clippy -D warnings`, `test`, `deny`), `docs/schema.md`, and **stubs for
every shared seam**: protocol message types and codec signatures, store
repository method signatures, config struct. Everything compiles; bodies are
`todo!()`.

Nothing else may start until the seams exist, and nothing in M0 implements
behaviour — it exists purely so four agents can then work without blocking.

### M1 — It tunnels *(done; WS1 ∥ WS3)*

Hardcoded token, in-memory tunnel map, self-signed cert, no dashboard, no
database. `curl https://<slug>.localhost` reaches a local service and streams
back. WebSocket works.

This is proof of life and the highest-value milestone — everything after it is
addition rather than discovery.

### M2 — Real auth and state *(done; WS2, then WS3 swaps in)*

SQLite, users, sessions, login, token CRUD, dashboard pages. WS3 replaces the
hardcoded token with a store lookup plus the bind ACL, and wires revocation to
session termination.

### M3 — Production surface *(partial; WS4)*

ACME DNS-01 with hot reload, BYO cert, health probes, `install-service`,
`doctor`, per-user traffic feed.

Done: BYO certificate loading and 30-second file-change checks for new TLS
handshakes, plus the per-user traffic feed delivered with M2. Still needed:
ACME DNS-01 wildcard issuance and renewal with hot certificate replacement;
outbound-only agent sidecar probes; `install-service`; and `doctor`.

### M4 — Distribution *(partial; WS5)*

The [staging pipeline](releases.md) is live. Image, compose, chart,
tag-triggered production deployment, signed manifest, and install script
remain.

---

## Production cutover gates

These gates are for replacing the running Go relay. They are ordered by what
must be learned or built first. Passing staging smoke tests alone does not
clear them.

1. **No migration (decided).** The Go relay has a single user, so nothing is
   imported. Production starts from an empty database: bootstrap a new admin,
   mint new agent tokens, re-reserve any names, and switch agents to the
   `vorp-agent/1` binary. No migration tooling is needed.
2. **Fresh edge and protocol review (done 2026-10-05).** Every October 1
   review finding was rechecked against current code. Most were already fixed
   by later commits (rate and connection limits, slow-client timers, WebSocket
   slot release, revocation race, machine-identity collisions, ALPN fallback,
   chunked responses, single-write frames, a single subdomain validator).
   This pass adds TCP keepalive so vanished WebSocket clients release their
   slots, and per-user traffic history. It also writes down the remaining
   policies: no body byte limit (streaming plus the bandwidth quota), no
   trusted-proxy setting (the relay must be DNS-only), and yamux default
   windows. Rejected as not worth churning: moving the dashboard runtime
   traits out of `vorp-web`.
3. **Smoke test on the new production host (scoped down, decided
   2026-10-05).** Production runs on a **new host** alongside the Go relay;
   the cutover is a DNS switch. Since the operator is the only user, the full
   load exercise is replaced by a check on the real host, through its own
   hostname before DNS moves: a large streamed upload and download, SSE, a
   WebSocket, an agent reconnect, and token revocation. Record memory and
   connection counts while doing it.
4. **Minimal data safety.** SQLite on the host's persistent disk plus a
   nightly `sqlite3 .backup` systemd timer, with one restore tried. Retention
   policy, monitoring and alerting stay deferred until there are other users.
5. **Manual promotion.** Copy the artifact that passed staging for the same
   commit, verify its checksum, install it with a production unit derived from
   [`deploy/vorp-staging.service`](../deploy/vorp-staging.service), and obtain
   the production wildcard certificate with the [Certbot procedure](certificates.md).
   The tag-triggered release workflow moves to v0.1 below.
6. **Switch.** Mint new agent tokens and move your agents to the new `vorp`
   binary, change the production DNS records, and keep the Go host running as
   the rollback target until the new relay has run cleanly for a few days.

**Next:** gates 3–6. Needs the new host's provider, OS, architecture and SSH
access. The work is host setup, not code.

---

## v0.1 — open-source release

Goal: a stranger (or an AI agent acting for them) goes from nothing to a
working relay with one install command and one setup command, and from a fresh
laptop to a live tunnel with two commands. Decisions taken 2026-10-05: **MIT
license**, **built-in ACME is in v0.1**, and **native services are the default**
for keeping things running, with pm2 and containers as cookbooks.

Order matters: ACME first, because `setup` and `doctor` sit on top of it.

1. **Project basics.** `LICENSE` (MIT), `SECURITY.md`, `CONTRIBUTING.md`,
   `CHANGELOG.md`. Publishing is blocked until the license exists.
2. **Built-in ACME DNS-01** (`crates/relay/src/acme/`, `instant-acme`):
   wildcard issuance for `domain` + `*.domain`, a renewal task, hot
   replacement through the existing certificate-reload path, an `AcmeDns`
   trait with Cloudflare first. Supplied cert files remain the alternative.
   A wildcard is required because tunnel names are random; per-name
   TLS-ALPN-01 certificates would hit Let's Encrypt's 50-per-week limit.
3. **Settings from the environment.** Every `serve` flag also reads a
   `VORP_*` variable (clap `env`), so systemd, Docker and agents share one
   `/etc/vorp/vorp.env`. No new config-file format.
4. **`vorp doctor [--json]`.** Checks DNS for the root and wildcard, that :443
   is free or held by vorp, the DNS-provider token, certificate validity, and
   database access. Machine-readable output with stable check ids so an agent
   can act on failures.
5. **`vorp setup`.** Non-interactive with flags (`--domain`,
   `--cloudflare-token-file`, `--admin-email`, `--yes`): runs `doctor`, gets
   the certificate, writes `/etc/vorp/vorp.env`, installs and starts the
   service, creates the admin, and prints the dashboard URL.
6. **Close the bootstrap race.** Today the first visitor to the dashboard
   becomes admin. On a public install, the admin is created from the command
   line by `setup` (or the relay prints a one-time setup code that the
   bootstrap page requires).
7. **`vorp service install`** for the relay (system unit, hardened like the
   staging unit) and the agent (systemd user unit with linger on Linux,
   launchd on macOS).
8. **Agent first-run experience.** `vorp login <relay>` stores the token and
   the relay host once (the token is read from a prompt or standard input,
   never an argument). `vorp http <port> [--name <reserved>]` opens a tunnel
   to `127.0.0.1:<port>`. The existing flags keep working.
9. **Release workflow.** On an annotated `vX.Y.Z` tag: static binaries for
   Linux and macOS on amd64 and arm64, checksums, and a signed manifest
   (minisign or cosign, not GPG). Then the production promotion described in
   [releases.md](releases.md).
10. **`install.sh`.** Detects OS and architecture, verifies the signature and
    checksum, installs to `/usr/local/bin`. Same script for the relay and the
    agent.
11. **Docs.** A README quickstart; `docs/setup.md`, written as a runbook with
    exact commands, expected output and what to do for each `doctor` failure,
    for people and AI agents alike; and cookbooks in `docs/cookbook/`:
    systemd, launchd, pm2 (token file only: a `--token` argument shows in `ps`
    and in pm2's saved process list), Docker Compose sidecar
    (`--allow-remote-targets --upstream http://app:3000`). User-facing docs
    must say a tunnel URL is not access control.

### v0.2

- Several named tunnels from one agent process (`tunnels.toml`, `vorp start
  [name]`); today one agent process serves one upstream.
- Container image and Compose file for the relay; Homebrew tap.
- CLI login approved in the dashboard instead of copying a token.
- Kubernetes: agent sidecar probes (outbound-only) and a Helm chart.
- API tokens and an agent-facing API description (deferred 2026-10-05 until
  the dashboard work settled; the flow is still to be designed).

---

## Parallelism map

```
M0 ─┬──> WS1 (protocol + agent) ─┬──> M1 ─┬──> WS4 (integrations) ──> M3 ─┐
    │                            │        │                               ├──> M4
    ├──> WS3 (relay) ────────────┘        └──> WS5 (deployment) ──────────┘
    │
    └──> WS2 (store + web) ──────────────────> M2
```

- WS1 and WS3 both consume `protocol`; **WS1 owns it**, WS3 only imports.
- WS3 consumes `store`; **WS2 owns it**, WS3 only imports. WS2's first deliverable
  is the token/subdomain methods so WS3 is never waiting.
- WS2 is independent of M1 entirely — it can run from M0 to completion in
  parallel with the tunnel work.
- `src/main.rs` is touched by WS4 only. WS1/WS3 expose library entry points and
  do not wire the CLI.

## Conflict rules

- Touching a path another workstream owns: don't. Ask for the change instead.
- Changing a shared seam (protocol type, store signature): it is a spec change —
  update `docs/protocol.md` or `docs/schema.md` in the same commit and say so, so
  the other workstreams see it.
- Adding a dependency: it passes `cargo deny`, or it does not go in.
- A workstream is done when its gates pass **and** its tests cover the branches
  `AGENTS.md` requires, not when the happy path works.
