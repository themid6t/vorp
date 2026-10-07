# vorp — Development Ground Rules

## Project

`vorp` is a self-hostable reverse tunnel relay (like ngrok), written in Rust and
designed to be set up in one shot with no external services. **One binary, two
modes:** `vorp serve` runs the relay, `vorp` (agent mode) runs on a developer
machine or as a Pod sidecar. The relay owns a single `:443` rustls listener and
routes on **ALPN** — `vorp-agent/1` goes to the yamux session handler, `h2` /
`http/1.1` goes to axum (embedded dashboard + `/api`, and the public tunnel
proxy, dispatched by `Host`). There is no nginx: the relay terminates its own
TLS and obtains a wildcard certificate itself via ACME DNS-01. State lives in
SQLite in-process. Auth is local email/password with argon2id and opaque
server-side sessions. The agent **only dials outbound** — it never accepts an
inbound connection, which is what lets it run behind NAT.

Rules below that exist because of a specific bug or decision say so; that
"why" is what stops them being undone.

---

## Non-goals (do not add these back)

Each was considered and deliberately excluded. Reopening one needs a reason in
the PR description, not a drive-by commit.

- **No Postgres.** The tunnel and session maps are in-memory, so a second relay
  instance cannot route to the first's agents — there is no horizontal scaling
  story for Postgres to enable yet. SQLite only until a multi-node design exists.
- **No JWT / JWKS.** Opaque server-side sessions, killable from the server.
  No external identity provider.
- **No CORS, no `dashboard_origin`.** The dashboard is same-origin and embedded.
  If you find yourself adding an `Access-Control-Allow-Origin` header, something
  upstream is wrong.
- **No orgs / teams.** Users and an admin role, nothing above that.
- **Per-user quotas are policy, separate from survival caps.** Each non-admin
  user may hold a limited number of live tunnels (default 3, set per user by an
  admin). Traffic limits apply per user, across all of that user's tunnels.
  Per-tunnel and global concurrency caps remain survival bounds and are not a
  substitute for the user quota. Keep the limit values on the session/tunnel
  structs, populated from the user's stored policy, not global consts.
- **No protocol version-branching machinery.** Send the version, reject a
  mismatch. Build a branch when a V2 actually exists, not before.
- **No QUIC.** UDP/443 is blocked on enough networks that it needs a TCP
  fallback, i.e. two transports to maintain. yamux-over-TLS is proven here.
- **No nginx, no pm2, no separate dashboard deploy.** One binary, one process.

---

## Async and concurrency

- **tokio.** Every task that can outlive a single request takes a
  `CancellationToken` (or equivalent) and `select!`s on it. No detached
  `tokio::spawn` that ignores shutdown — a task with no cancellation path is a
  leak and a hang on shutdown.
- **Graceful shutdown propagates to listeners.** Cancellation must reach the
  accept loops so they stop accepting and in-flight work drains; a listener that
  only dies when the process does is a bug.
- **The yamux connection driver must keep polling while a stream can block.**
  Do not await a per-stream read, write, or open operation in the task that
  polls the connection; a stalled stream otherwise freezes every tunnel on the
  session. Keep the driver running independently and cancel it on teardown.
- **Split a yamux stream only with `vorp_protocol::split`, never
  `tokio::io::split`.** A `yamux::Stream`'s reads (window updates) and writes
  park on one channel sender that keeps a single waker, so a reader polled from
  another task overwrites a parked writer's waker and the writer never wakes.
  This deadlocked every HTTP/2 upload over ~150 KiB. `vorp_protocol::split`
  wakes both halves on any inner wake.
- **Guard every map removal with `Arc::ptr_eq`.** Session and tunnel maps hold
  `Arc<T>`; remove an entry only when the stored `Arc` is still *the same
  allocation* you are tearing down (delete-if-match). It exists because of a
  real bug class: an agent reconnects (or a tunnel
  re-registers after a recoverable close) and stores a replacement under the same
  key *before* the old session's teardown runs — an unguarded `remove` then
  deletes the live replacement.
- **Teardown runs exactly once** per session, from whichever task gets there
  first (`OnceCell` / `AtomicBool` / `tokio::sync::OnceCell`), and is safe to call
  concurrently.
- Prefer `Arc<Mutex<HashMap<..>>>` (or `dashmap`) over hand-rolled sharding until
  a profile says otherwise.

---

## The streaming rule (hard rule)

**Proxied request and response bodies are streamed, never buffered.**

- Bodies travel as length-delimited chunks with an explicit end marker, behind a
  structured header frame.
- **No `read_to_end`, no `collect()`, no `to_bytes()`, no `Vec<u8>` sized to a
  declared content length** on a proxied body — in either direction, on either
  side of the tunnel. If a reviewer sees a body materialized whole, that is a
  rejection.
- **Backpressure must propagate** browser ↔ relay ↔ agent through yamux's stream
  window. The rule in practice: pass the stream through, never drain it into an
  intermediate buffer. A slow reader must slow the writer, not grow a buffer.
- Why this is a rule and not a preference: buffering each whole message costs
  a full in-memory copy of every body, makes SSE through a tunnel unusable
  (responses sit in a buffer until a deadline fires), and turns the body-size
  cap into a memory-exhaustion survival knob instead of a policy choice.
- Body size limits remain, but as **policy** (reject early with 413), not as the
  thing standing between the relay and OOM.
- WebSocket (HTTP 101) is the exception to the request/response shape, not to this
  rule: after the upgrade, clear deadlines and copy bidirectionally.

---

## Errors

- `thiserror` for library and domain error enums. `anyhow` **only** at binary
  edges (`main`, top-level task bodies).
- **No `unwrap()` / `expect()`** outside tests and genuinely-infallible startup
  invariants — and an infallible one carries a comment saying why it cannot fail.
- Add context when crossing a module boundary; a bare `?` chain that surfaces
  "invalid digit found in string" with no provenance is not an error message.
- **Never silently swallow an error.** A deliberately ignored error gets
  `let _ = ...` *and* a comment saying why it is irrelevant.
- Domain conditions callers must branch on get their own variant, matched with
  `matches!` / `if let` — never string matching on a message.

---

## Logging

- **`tracing` only.** No `println!`, no `eprintln!`, no `dbg!` in committed code.
- **Structured fields, never format strings:**

  ```rust
  // good
  tracing::info!(machine_id = %machine_id, session_id = %session_id, "agent connected");
  tracing::error!(addr = %addr, error = %err, "dial failed");

  // bad
  tracing::info!("agent {} connected with session {}", machine_id, session_id);
  ```

- `info` for lifecycle events, `warn` for recoverable issues, `error` for
  failures. A `warn` that fires on every request is an `info` or a bug.
- **Never log a secret:** no agent tokens (raw or hashed), no passwords, no
  session ids, no cookie values, no `Authorization` header contents. Log the
  token's *id*, never its value.
- Use `#[tracing::instrument(skip(..))]` and skip anything carrying a secret or a
  body.

---

## Security

These are load-bearing; each one closes a specific hole. Do not relax one
without replacing what it defends.

### Credentials

- **Agent tokens:** high-entropy random from a CSPRNG, stored as a **SHA-256 hash
  only**, raw value shown **exactly once** at creation/rotation and never stored.
  Compare in **constant time**. **Do not argon2 an agent token** — the value is
  high-entropy random, not a human password, so a KDF buys nothing and costs
  latency on every agent registration.
- **User passwords:** **argon2id**, never anything else, never a bare hash.
- **Sessions:** opaque random id in a server-side `sessions` table, delivered as
  `HttpOnly; Secure; SameSite=Lax`. Logout and password change delete server-side
  rows — a session must be killable from the server.
- **Development tokens are loopback-only and self-signed-only.** This mode
  bypasses the database token lookup and bind ACL for local smoke tests. Both
  constraints are required; neither one alone makes it safe on a public relay.
- Revoking or rotating an agent token must **terminate the live agent sessions
  already authenticated with it**, not just future ones. Each session records its
  authenticating token id; revoke → close that token's sessions' tunnels with a
  permanent reason → tear the sessions down. Without this, a revoked token
  keeps a live tunnel.

### Proxy boundary

- **The relay is the edge.** With nginx removed, it must bound concurrent
  connections, slow request headers, and expensive authentication attempts
  itself. Streaming prevents body buffering, not bandwidth or CPU exhaustion.

- **HTTP normalization is mandatory** (`crates/relay/src/httpnorm.rs`):
  - **Reject ambiguous body framing with `400`** — a message carrying both a
    `Content-Length` and a `Transfer-Encoding`, multiple disagreeing
    `Content-Length` values, a non-numeric length, or any transfer coding other
    than `chunked`. This is the request-smuggling vector; reject before the
    message can reach the local service.
  - **Strip hop-by-hop headers** (`Connection`, `Proxy-Connection`, `Keep-Alive`,
    `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`,
    `Transfer-Encoding`, `Upgrade`) **plus every header named in the sender's own
    `Connection` header** (RFC 7230 §6.1), in **both** directions. Remove
    `Content-Length` too: the body length travels in the frame head, and
    each side derives its own outgoing framing. `Upgrade`
    survives **only** for a validated WebSocket handshake (`Upgrade: websocket`
    plus an `upgrade` token in `Connection`); every other upgrade is dropped.
  - **Assert forwarding headers, never trust them.** Overwrite
    `X-Forwarded-For`, set `X-Forwarded-Host` from the requested host, and pin
    `X-Forwarded-Proto`. A client must not be able to spoof the proxy chain or its
    own identity.
- **Subdomain slugs come from a CSPRNG with no weak fallback.** A CSPRNG failure
  **fails the tunnel registration** — never fall back to a non-crypto RNG; a
  predictable slug generator is a hardening hole.
  **A high-entropy URL is not access control:** a live tunnel is reachable by
  anyone holding the URL, so it is never a substitute for authentication in the
  tunneled application. Say this in user-facing docs too.
- **Agent upstreams fail closed.** Non-loopback upstream targets require an
  explicit opt-in flag; reject a target with a path, query, fragment or userinfo.
  This is the SSRF / cluster-exposure guard.

### Authorization

- **The bind ACL decision stays a pure function** (`decide_bind`): it takes a fully-resolved context value (requested name, user id,
  token policy, allowlist, assigned name, reservation owner) and returns
  allow-or-typed-rejection. **No I/O inside it.** The caller resolves state, the
  function decides. This is what makes the policy table testable.
- **The live traffic feed is authenticated and scoped per-user.** A feed that
  is unauthenticated or global across users leaks every user's traffic,
  especially for a company hosting this internally. Every `/api/*` route requires a session
  except the necessary entry points (`/api/bootstrap`, `/api/signup`, and
  `/api/login`); bootstrap is one-shot and signup follows the configured mode.
  `/healthz` is public and sits outside `/api`.
- **Mutating dashboard APIs require the `X-Vorp-Csrf: 1` header.** Same-origin
  JavaScript can set it; a cross-origin form cannot, and there is deliberately
  no CORS preflight permission. Authentication cookies alone are insufficient.
- Owner-or-admin checks are enforced **in the query** (`where user_id = ?`), not
  by filtering after the fetch.

---

## Code structure

- **One job per function.** If a comment labels a phase inside a function body,
  that phase is its own function.
- **Business logic separate from I/O.** A policy decision (`decide_bind`, slug
  validation, framing validation) must be callable without a socket or a database.
- **Methods on the owning type**, not free functions taking the world. Relay
  behavior belongs on `Relay`/`Server`, not a function receiving
  `(&agent_map, &tunnel_map, &cfg)`.
- **All SQL behind one repository module.** No `sqlx::query!` scattered through
  handlers — a second backend (or a schema change) must stay contained to one
  place. Handlers call repository methods.
- **Pure helpers live in their own module** (`util.rs` / `subdomain.rs`), not
  alongside handler logic.
- Prefer unexported (`pub(crate)` or private) items; export only what a consumer
  needs.
- **Accurate naming.** If it is base32, do not call it `base62`. If a field holds
  a token id, it is `token_id`, not `token`. No stutter: `AgentSession::id`, not
  `AgentSession::agent_id`.
- **One concern per file.** Do not pile unrelated functions into a module because
  they happen to be called together.
- `defer`-equivalent at acquisition: rely on RAII guards, and make cleanup a
  `Drop` impl or a guard object rather than a `Close()` scattered across
  early-return paths.

---

## Testing

- Unit tests live next to the code, in `#[cfg(test)] mod tests`. `#[tokio::test]`
  for async.
- **Every non-trivial branch, parser, and security path gets a test.** Framing
  validation, header stripping, slug validation, auth — if it can be wrong, it has
  a test that fails when it is.
- **Table-driven tests are required for two things specifically:**
  1. **Protocol framing** — round-trip, truncated frame, oversized declared
     length, unknown message type, chunk/end-marker sequencing.
  2. **The bind ACL decision table** — every (policy × requested-name ×
     reservation-owner) combination with its expected rejection code
     (`crates/relay/src/authz.rs` is the template).
- Tests run under `--all-features`; a feature that is never compiled in CI is
  dead code.
- Concurrency-sensitive code (the session/tunnel maps, teardown) gets a test that
  exercises the race — the reconnect-displaces-session path in particular.
- No mocking frameworks. Inject a function or a small trait where a seam is
  genuinely needed (e.g. the slug generator, so CSPRNG failure and collision
  retry are testable).

---

## Tooling and CI gates

All of these must pass for changes beyond Markdown; CI enforces them and a red
gate is not merged. Markdown-only changes keep a successful lightweight check
and skip the Rust gates and staging deployment. The dashboard is built first,
because the binary embeds it:

```sh
(cd crates/web/ui && npm ci && npm run check && npm run build)
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check          # or, at minimum, cargo audit
```

- **Pin the toolchain** in `rust-toolchain.toml` (channel + components), and pin
  the edition in `Cargo.toml`. A build that depends on whatever the developer
  happens to have installed is not reproducible.
- `#[allow(..)]` needs a comment giving the reason, scoped as narrowly as
  possible. A crate-level `allow` for a lint you did not want to fix is not
  acceptable.
- Release binaries build with `CARGO_PROFILE_RELEASE_*` set for a static,
  trimmed, reproducible artifact — the agent ships as a single file.

## Dashboard

- **Svelte 5 + Vite + TypeScript in `crates/web/ui`, embedded with
  `rust-embed`.** Node is a build-time tool only; the relay still ships as one
  binary and serves everything itself. Debug builds read `ui/dist` from disk,
  so `npm run dev` (proxying `/api` to a local relay) or a rebuilt `dist`
  shows up without recompiling.
- **Nothing loads from the internet.** Fonts and every other asset are bundled
  by Vite; no CDN, no analytics, no remote images.
- **The CSP stays strict** (`script-src 'self'; style-src 'self'`). No inline
  `<script>`, no `style="..."` attributes, no `{@html}` — API data is rendered
  through Svelte interpolation only, so it cannot inject markup.
- **All API calls go through `src/lib/api.ts`**, which sets the CSRF header
  and turns relay errors into messages. Shared state, notices, confirmations
  and routing live in `src/lib/app.svelte.ts`; pure helpers in
  `src/lib/format.ts`.
- **Neumorphism with guardrails.** Colours and shadows are tokens in
  `src/theme.css`, defined for light and dark. Keep text contrast at 4.5:1 or
  better, give the primary action a solid accent fill, keep focus rings
  visible, and pair every status colour with text; embossed-only controls are
  indistinguishable for many users.

## Release and certificate operations

- Use one long-lived `main` branch. Pull requests with non-Markdown changes run
  the four CI gates; a passing push to `main` with such changes builds and
  deploys to staging automatically. The current pipeline is documented in
  `docs/releases.md`.
- An annotated `vX.Y.Z` tag publishes a release (`release.yml`): it promotes
  the binaries CI built for that commit, which staging already ran, and never
  rebuilds on the tag. It signs the manifest and uploads the release to the
  `get-vorp` S3 bucket and a GitHub Release. `install.sh` installs from there.
  Bump `version` in the root `Cargo.toml` before tagging; the workflow rejects
  a mismatch.
- Production is upgraded by hand with the installer. Automatic production deploys are not configured; if added,
  they need credentials separate from staging. Operator details for specific
  hosts stay out of this repo.
- Until Vorp implements its own ACME DNS-01 issuer, deployed relays use
  supplied certificate files. Staging obtains and renews its wildcard cert
  through Certbot and the Cloudflare DNS plugin. Preserve the renewal hook and
  Vorp's certificate-file reload behavior; see `docs/certificates.md`. Never
  place the Cloudflare token, TLS private key, or agent tokens in the repo or
  GitHub Actions artifacts.

---

## Extending this file

Living document. It gets refined and added to as the project grows — when a
decision is made twice, write it down here; when a bug turns out to have been
preventable by a rule, add the rule *and* the half-sentence of why.

`CLAUDE.md` is a **symlink** to this file. Edit `AGENTS.md`; never replace the
symlink with a copy.
