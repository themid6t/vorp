# SQLite schema and repository seam

Status: SQLite repository implemented. The persistence workstream owns migrations and all SQL in
`crates/store`. Other crates call `Repository` methods, never SQL directly.

## Tables

All timestamps are Unix milliseconds. Foreign keys are enabled on each SQLite
connection. The migration must enable WAL mode and a busy timeout. Table names
and columns below are the contract; migration SQL is owned by the store crate.

| Table | Columns and constraints |
|---|---|
| `users` | `id INTEGER PRIMARY KEY`, `email TEXT NOT NULL UNIQUE COLLATE NOCASE`, `password_hash TEXT NOT NULL` (argon2id PHC string), `is_admin INTEGER NOT NULL CHECK (is_admin IN (0,1))`, `assigned_subdomain TEXT UNIQUE` (nullable until first assignment), `created_at_ms INTEGER NOT NULL` |
| `sessions` | `id TEXT PRIMARY KEY` (lowercase hex SHA-256 of the opaque CSPRNG cookie value; the raw value is never stored), `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `expires_at_ms INTEGER NOT NULL`; index on `user_id` and expiry |
| `agent_tokens` | `id INTEGER PRIMARY KEY`, `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash)=32)`, `bind_policy TEXT NOT NULL CHECK (bind_policy IN ('any','temporary','reserved'))`, `created_at_ms INTEGER NOT NULL`, `revoked_at_ms INTEGER`; index on `user_id` |
| `token_allowlist` | `token_id INTEGER NOT NULL REFERENCES agent_tokens(id) ON DELETE CASCADE`, `name TEXT NOT NULL`, `PRIMARY KEY (token_id,name)` |
| `reserved_subdomains` | `name TEXT PRIMARY KEY`, `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `created_at_ms INTEGER NOT NULL`; index on `user_id` |
| `settings` | `key TEXT PRIMARY KEY`, `value TEXT NOT NULL` for optional persisted deployment settings; signup mode currently comes from the relay CLI |
| `user_limits` | `user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE`, nullable `max_tunnels`, `bandwidth_bytes_per_sec`, `max_concurrent_requests` (each `CHECK (>0)`); an admin's per-user quota overrides, where `NULL` inherits the default |
| `signup_invites` | `id INTEGER PRIMARY KEY`, `created_by INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `code_hash BLOB NOT NULL UNIQUE CHECK(length(code_hash)=32)`, `created_at_ms INTEGER NOT NULL`, `used_at_ms INTEGER`; one-time admin-created invite codes |

Bind policy follows the legacy `decideBind` decision table: `any` permits
temporary tunnels, the user's assigned name, and reserved names they own;
`temporary` permits only temporary tunnels; `reserved` permits only owned
reserved names listed in that token's `token_allowlist`. Only `reserved` tokens
may have allowlist entries. The assigned name belongs to the user, not a token.
It is generated from a CSPRNG, stored uniquely, and returned by
`get_or_create_assigned_subdomain`. Reservation creation must check assigned
name ownership so another user cannot reserve it.

`agent_tokens.token_hash` contains SHA-256 of a high-entropy raw token. The raw
token includes its public token ID as a prefix, is returned by the creation API
once, and is never persisted. Authentication looks up by ID and compares the
stored hash in constant time. Session IDs are sensitive and
must never be logged. Revocation writes `revoked_at_ms`; the caller then invokes
the relay's `disconnect_token(token_id)` to terminate already authenticated
sessions. Active tunnel and traffic state stays in memory, scoped by `user_id`.

## Repository signatures

The signatures in `crates/store/src/repository.rs` are authoritative. The M0
baseline defined:

```text
open(path) -> Repository
create_user(NewUser) -> User
user_by_email(email) -> Option<User>
get_or_create_assigned_subdomain(user_id) -> String
assigned_subdomain_owner(name) -> Option<user_id>
create_session(NewSession) -> ()
session_by_id(id) -> Option<Session>
delete_session(id) -> ()
delete_user_sessions(user_id) -> ()
create_token(NewAgentToken) -> TokenRecord
token_by_id(token_id) -> Option<TokenRecord>
tokens_for_user(user_id) -> Vec<TokenRecord>
revoke_token(token_id, owner_user_id) -> bool
reservation_owner(name) -> Option<user_id>
reserve_subdomain(name, owner_user_id) -> ReservedSubdomain
release_subdomain(name, owner_user_id) -> bool
```

Every method returns `Result<_, RepositoryError>` and is async. Scoped writes
must include `user_id` in the SQL predicate. Admin access should use explicit
admin methods added in this module, never a fetch-then-filter in a handler.
The store workstream may extend these signatures alongside a schema doc update.

## Implemented extensions

Per-user quotas: `default_limits` returns the deployment-wide defaults, stored
in `settings` under `limits.max_tunnels`, `limits.bandwidth_bytes_per_sec`
and `limits.max_concurrent_requests`, falling back to `UserLimits::DEFAULT`
(3 tunnels, 10 MiB/s, 64 requests). `set_default_limits` writes all three.
`set_limit_overrides(user_id, overrides)` replaces a user's overrides, and
all-`None` deletes the row. `effective_limits(user_id)` merges the two and
returns `None` for an admin, who is exempt. `users_with_limits` lists every
user with their overrides for the admin dashboard; the handler checks the
caller is an admin. Every value must be at least 1.

`user_by_id`, `user_count`, and transactional `bootstrap_admin` support local
account setup. `update_password` changes the hash and deletes all of that user's
sessions in one transaction. `setting` and `set_setting` are available for
persisted deployment options; bootstrap is determined by the users table and
signup mode is set with `vorp serve --signup`.

`mint_token(user_id, policy, allowlist)` allocates an ID, generates 32 random
bytes, and returns `vorp_<id>_<hex-secret>` exactly once. Only its SHA-256 hash
is stored. `authenticate_token(raw)` looks up the ID, compares the complete raw
token's hash in constant time, and rejects revoked tokens. `token_for_user`
supports scoped revocation retries. All SQLite operations run on Tokio's
blocking pool behind one serialized connection.

`mint_invite(admin_user_id)` creates a one-time code after checking the caller
is an admin in SQL. `create_user_with_invite` validates its SHA-256 hash in
constant time and creates the user while marking the code used in a single
immediate transaction. In `signup_mode=invite`, the signup API requires this
code; `closed` disables self-registration.

Reservations must have 3–63 lowercase ASCII letters, digits, or hyphens, with
an alphanumeric edge. `www`, `api`, `mail`, `smtp`, `ftp`, `admin`, `dash`,
`dashboard`, and `vorpd` are reserved. `reservations_for_user` is scoped in SQL.

The web crate exposes `router_with_disconnect` for the relay to provide a
`TokenDisconnect` callback. The token revocation API fails closed when no
callback is configured, and retries disconnect even when the token was already
marked revoked. The dashboard's cookie is `HttpOnly; Secure; SameSite=Lax`; API
mutations require `X-Vorp-Csrf: 1` from same-origin JavaScript.

The web crate also exposes `DashboardRuntime` for the relay to provide
per-user tunnel snapshots, owner-scoped force-close, and recent traffic. The
HTTP handlers derive `user_id` from the server-side session and pass it into
the runtime; `/api/traffic/recent` and `/api/traffic/stream` cannot return a
global feed. They return 503 until the relay supplies the runtime hook.
