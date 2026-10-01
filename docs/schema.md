# SQLite schema and repository seam

Status: M0 contract. The persistence workstream owns migrations and all SQL in
`crates/store`. Other crates call `Repository` methods, never SQL directly.

## Tables

All timestamps are Unix milliseconds. Foreign keys are enabled on each SQLite
connection. The migration must enable WAL mode and a busy timeout. Table names
and columns below are the contract; migration SQL is owned by the store crate.

| Table | Columns and constraints |
|---|---|
| `users` | `id INTEGER PRIMARY KEY`, `email TEXT NOT NULL UNIQUE COLLATE NOCASE`, `password_hash TEXT NOT NULL` (argon2id PHC string), `is_admin INTEGER NOT NULL CHECK (is_admin IN (0,1))`, `assigned_subdomain TEXT UNIQUE` (nullable until first assignment), `created_at_ms INTEGER NOT NULL` |
| `sessions` | `id TEXT PRIMARY KEY` (opaque CSPRNG value), `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `expires_at_ms INTEGER NOT NULL`; index on `user_id` and expiry |
| `agent_tokens` | `id INTEGER PRIMARY KEY`, `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash)=32)`, `bind_policy TEXT NOT NULL CHECK (bind_policy IN ('any','temporary','reserved'))`, `created_at_ms INTEGER NOT NULL`, `revoked_at_ms INTEGER`; index on `user_id` |
| `token_allowlist` | `token_id INTEGER NOT NULL REFERENCES agent_tokens(id) ON DELETE CASCADE`, `name TEXT NOT NULL`, `PRIMARY KEY (token_id,name)` |
| `reserved_subdomains` | `name TEXT PRIMARY KEY`, `user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE`, `created_at_ms INTEGER NOT NULL`; index on `user_id` |
| `settings` | `key TEXT PRIMARY KEY`, `value TEXT NOT NULL` for one-shot bootstrap state and signup mode |

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

The signatures in `crates/store/src/repository.rs` are authoritative. M0 defines:

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
