# Dashboard API

The embedded dashboard is same-origin with this API; there is no CORS. All
bodies are JSON. Authentication is the `vorp_session` cookie
(`HttpOnly; Secure; SameSite=Lax`), set by bootstrap, signup and login.

- Every `/api/*` route needs a session except `bootstrap`, `signup` and
  `login`. Missing or expired sessions get `401`.
- Every mutating request (`POST`, `PUT`, `DELETE`) must send
  `X-Vorp-Csrf: 1`, or it is rejected with `403`.
- Admin-only routes return `403` to a non-admin.
- Errors are `{"error": "<message>"}` with `400` (invalid input, unknown
  record), `401`, `403`, `409` (conflict), `429` (rate limited), or `503`
  (relay hook unavailable).

## Account

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `/api/bootstrap` | `{email, password}` | `{ok}`; creates the first admin, once |
| POST | `/api/signup` | `{email, password, invite_code?}` | `{ok}`; per `--signup` mode |
| POST | `/api/login` | `{email, password}` | `{ok}` |
| POST | `/api/logout` | — | `{ok}` |
| GET | `/api/me` | — | `{id, email, is_admin, assigned_subdomain, limits}` |
| POST | `/api/password` | `{old_password, new_password}` | `{ok}`; ends every session |

`limits` in `/api/me` is the user's effective quota
(`{max_tunnels, bandwidth_bytes_per_sec, max_concurrent_requests}`), or
`null` for an admin, who is exempt.

## Tokens, names, tunnels, traffic (scoped to the caller)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/api/tokens` | — | `[{id, bind_policy, allowlist, revoked}]` |
| POST | `/api/tokens` | `{bind_policy: "any"\|"temporary"\|"reserved", allowlist?}` | `{token, raw_token}`; `raw_token` is shown once |
| POST | `/api/tokens/{id}/revoke` | — | `{ok}`; also disconnects live agents |
| GET | `/api/reservations` | — | `[{name}]` |
| POST | `/api/reservations` | `{name}` | `{name}` |
| DELETE | `/api/reservations/{name}` | — | `{ok}` |
| GET | `/api/tunnels` | — | `[{subdomain, machine_id, upstream_hint, active_requests}]` |
| POST | `/api/tunnels/{name}/close` | — | `{ok}` |
| GET | `/api/traffic/recent` | — | `[{subdomain, timestamp_ms, method, status, bytes_in, bytes_out}]` |
| GET | `/api/traffic/stream` | — | SSE; each event is the recent-traffic array |

## Admin

| Method | Path | Body | Returns |
|---|---|---|---|
| POST | `/api/users` | `{email, password}` | the created user |
| POST | `/api/invites` | — | `{invite_code}` |
| GET | `/api/admin/limits` | — | defaults `{max_tunnels, bandwidth_bytes_per_sec, max_concurrent_requests}` |
| PUT | `/api/admin/limits` | all three fields | the stored defaults |
| GET | `/api/admin/users` | — | `[{id, email, is_admin, created_at_ms, overrides, effective}]` |
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
  sessions; retrying is safe.
