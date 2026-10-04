# Dashboard API

The embedded dashboard is same-origin with this API; there is no CORS. All
bodies are JSON. Authentication is the `vorp_session` cookie
(`HttpOnly; Secure; SameSite=Lax`), set by bootstrap, signup and login.

- Every `/api/*` route needs a session except `config`, `bootstrap`,
  `signup` and `login`. Missing or expired sessions get `401`.
- Every mutating request (`POST`, `PUT`, `DELETE`) must send
  `X-Vorp-Csrf: 1`, or it is rejected with `403`.
- Admin-only routes return `403` to a non-admin.
- Errors are `{"error": "<message>"}` with `400` (invalid input, unknown
  record), `401`, `403`, `409` (conflict), `429` (rate limited), or `503`
  (relay hook unavailable).
- An account whose `must_change_password` is set gets `403`
  `{"error": "password change required"}` from every route except `me`,
  `password` and `logout`. Admin-created accounts and host resets
  (`vorp admin reset-password`) set it; changing the password clears it.

## Account

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/api/config` | — | `{signup_mode: "open"\|"closed", needs_bootstrap, base_domain}`; public, read-only |
| POST | `/api/bootstrap` | `{email, password}` | `{ok}`; creates the first admin, once |
| POST | `/api/signup` | `{email, password}` | `{ok}`; `403` unless `--signup open` |
| POST | `/api/login` | `{email, password}` | `{ok}` |
| POST | `/api/logout` | — | `{ok}` |
| GET | `/api/me` | — | `{id, email, is_admin, assigned_subdomain, limits, must_change_password, can_reserve_directly}` |
| POST | `/api/password` | `{old_password, new_password}` | `{ok}`; ends every session, clears `must_change_password` |

`/api/config` lets the login screen choose between first-run setup, login and
signup before anyone has a session. `needs_bootstrap` is true only while no
user exists, which `POST /api/bootstrap` already reveals. Tunnels are served at
`https://<subdomain>.<base_domain>`.

`limits` in `/api/me` is the user's effective quota
(`{max_tunnels, bandwidth_bytes_per_sec, max_concurrent_requests}`), or
`null` for an admin, who is exempt. `can_reserve_directly` is always true for
an admin.

## Tokens, names, tunnels, traffic (scoped to the caller)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/api/tokens` | — | `[{id, bind_policy, allowlist, revoked}]` |
| POST | `/api/tokens` | `{bind_policy: "any"\|"temporary"\|"reserved", allowlist?}` | `{token, raw_token}`; `raw_token` is shown once |
| POST | `/api/tokens/{id}/revoke` | — | `{ok}`; also disconnects live agents |
| GET | `/api/reservations` | — | `[{name, status: "reserved"\|"pending"}]` |
| POST | `/api/reservations` | `{name}` | `{name, status}`; see below |
| DELETE | `/api/reservations/{name}` | — | `{ok}`; releases a reservation or withdraws a pending request |
| GET | `/api/tunnels` | — | `[{subdomain, machine_id, upstream_hint, active_requests}]` |
| POST | `/api/tunnels/{name}/close` | — | `{ok}` |
| GET | `/api/traffic/recent` | — | `[{subdomain, timestamp_ms, method, status, bytes_in, bytes_out}]` |
| GET | `/api/traffic/stream` | — | SSE; each event is the recent-traffic array |

Reservation requests: a user with `can_reserve_directly` (and every admin)
gets `status: "reserved"` at once. Anyone else gets `status: "pending"` until an
admin approves it; only a reserved name can go in a token allowlist or be bound
by an agent. A pending request holds the name, so a conflicting `POST` gets
`409` with a message naming the holder: already reserved by another user,
already requested by another user, another user's assigned subdomain, or
already reserved or requested by the caller.

## Admin

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `/api/users` | `{email, password}` | the created user; they must change the password at first login |
| GET | `/api/admin/limits` | — | defaults `{max_tunnels, bandwidth_bytes_per_sec, max_concurrent_requests}` |
| PUT | `/api/admin/limits` | all three fields | the stored defaults |
| GET | `/api/admin/users` | — | `[{id, email, is_admin, created_at_ms, must_change_password, can_reserve_directly, overrides, effective}]` |
| PUT | `/api/admin/users/{id}` | `{is_admin, can_reserve_directly}` (both required) | the stored values; `400` for removing your own admin role or the last admin |
| GET | `/api/admin/reservation-requests` | — | `[{name, user_id, email, created_at_ms}]`, oldest first |
| POST | `/api/admin/reservation-requests/{name}/approve` | — | `{name, user_id}`; `409` if the name was taken meanwhile |
| POST | `/api/admin/reservation-requests/{name}/reject` | — | `{ok}`; frees the name |
| PUT | `/api/admin/users/{id}/limits` | `{max_tunnels?, bandwidth_bytes_per_sec?, max_concurrent_requests?}` | the stored overrides |

Quota semantics (see [protocol §8](protocol.md)):

- Defaults apply to every non-admin user; each override field is either a
  number or `null` (inherit the default). `effective` is the merged result,
  `null` for admins.
- A `PUT` of overrides **replaces** all three fields: an omitted field is
  cleared back to the default. Send `{}` to remove every override.
- Every value must be at least 1; `bandwidth_bytes_per_sec` is bytes per
  second, uploads and downloads combined.
- Changes apply to live tunnels immediately. Lowering `max_tunnels` blocks
  new registrations but does not close tunnels already open.
- A `503` after a `PUT` means the value was saved but not yet pushed to live
  sessions; retrying is safe. A role change behaves the same way, since
  admins are exempt from quotas.
