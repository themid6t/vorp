# vorp build roadmap

How this gets built, split so several agents can work at once without colliding.

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
  `signup: open | invite | closed`, token CRUD (raw value shown once), reserved
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
- Agent `/healthz` + `/readyz` probes for the sidecar case.
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

### M0 — Scaffold *(serial, one agent, blocks everything)*

Workspace, five crates, `rust-toolchain.toml`, CI with all four gates
(`fmt`, `clippy -D warnings`, `test`, `deny`), `docs/schema.md`, and **stubs for
every shared seam**: protocol message types and codec signatures, store
repository method signatures, config struct. Everything compiles; bodies are
`todo!()`.

Nothing else may start until the seams exist, and nothing in M0 implements
behaviour — it exists purely so four agents can then work without blocking.

### M1 — It tunnels *(WS1 ∥ WS3)*

Hardcoded token, in-memory tunnel map, self-signed cert, no dashboard, no
database. `curl https://<slug>.localhost` reaches a local service and streams
back. WebSocket works.

This is proof of life and the highest-value milestone — everything after it is
addition rather than discovery.

### M2 — Real auth and state *(WS2, then WS3 swaps in)*

SQLite, users, sessions, login, token CRUD, dashboard pages. WS3 replaces the
hardcoded token with a store lookup plus the bind ACL, and wires revocation to
session termination.

### M3 — Production surface *(WS4)*

ACME DNS-01 with hot reload, BYO cert, health probes, `install-service`,
`doctor`, per-user traffic feed.

### M4 — Distribution *(WS5)*

Image, compose, chart, release pipeline, install script.

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
