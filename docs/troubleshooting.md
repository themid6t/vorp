# Troubleshooting

Find the symptom, check the cause, apply the fix. Log lines and error texts
below are quoted from the vorp source. Commands use `example.com` for the relay's
base domain and `myapp` for a tunnel name.

## Reading logs

Both modes log to standard output through `tracing`, one line per event, with
structured `key=value` fields:

```
2026-10-07T15:04:44.522115Z  INFO vorp_agent::session: tunnel registered subdomain=myapp url=https://myapp.example.com
```

Fatal errors print an `Error:` block and the process exits with status 1:

```
Error: agent stopped

Caused by:
    agent authentication failed
```

| Where it runs | Command |
| --- | --- |
| Relay (systemd) | `journalctl -u vorp -f`, or `journalctl -u vorp -n 100 --no-pager` |
| Agent (systemd user unit) | `journalctl --user -u vorp-myapp -n 100 --no-pager` |
| Agent (pm2) | `pm2 logs vorp-myapp --lines 100 --nostream` |
| Agent (Kubernetes sidecar) | `kubectl logs deploy/myapp -c vorp`; after a crash add `--previous` |
| Agent (container) | `docker logs <container>` |

- Log lines contain ANSI colour codes. Set `NO_COLOR=1` in the environment for
  plain text, or strip them with `sed 's/\x1b\[[0-9;]*m//g'`. Colour codes sit
  between a field name and its `=`, so `grep 'subdomain=myapp'` misses coloured
  lines; `grep 'tunnel registered'` works either way.
- `RUST_LOG=debug` shows more detail (default level is `info`). Connections
  dropped by the connection limits are only logged at `debug`
  (`connection limit reached`).
- Neither mode logs tokens, passwords, session ids or cookies.

## Agent exits at start

| Output | Cause | Fix |
| --- | --- | --- |
| ``upstream is not set: pass --upstream, set VORP_UPSTREAM, or add `upstream` to the config file`` | No upstream. | Add `--upstream http://127.0.0.1:PORT`. |
| `/home/you/.config/vorp/config.yaml:2: unknown key ...`, `... is a secret ...`, or another message starting with the config file's name | The agent config file has a typo, a wrong type, or a `token:` key. | Fix that line. The token goes in its own file (`vorp login`), never in the config. `vorp config show` prints what is in effect. |
| `config file ... does not exist` | `--config` or `VORP_CONFIG` names a missing file. | Fix the path. Only the default config file may be absent. |
| `agent configuration is invalid` / `non-loopback upstream requires explicit opt-in` | The upstream host is not `localhost`, `127.0.0.0/8` or `::1`. | Use a loopback address, or add `--allow-remote-targets` if the service really is on another host (a container or Kubernetes Service name). |
| `upstream URL is invalid: expected an origin with no userinfo, path, query or fragment` | The upstream has a path such as `/api`, a query, or `user@`. | Give only `http://host:port`. The request path is forwarded as is. |
| `only http upstreams are supported` | `https://` upstream. | Point the agent at the service's plain HTTP port. |
| `read agent token from /home/you/.config/vorp/authtoken` / `No such file or directory (os error 2)` | No saved token. | Run `vorp login vorp.example.com` (or `vorp authtoken`), or pass `--token-file`. |
| `agent token is empty` | The token file or `VORP_TOKEN` is empty. | Save the token again. |
| `agent connection failed: no machine MAC address found` | No network interface with a MAC address, for example a container with `--network none`. | Run the agent with a normal network interface. |

## Agent connects, then exits

| Output | Cause | Fix |
| --- | --- | --- |
| `agent authentication failed` | The relay answered `AUTH_FAILED`: the token is wrong, revoked, or from another relay. Also printed after a live revocation. | Create a new token, save it, restart the agent. If `VORP_TOKEN` is set, it overrides `--token-file`; unset it. |
| `relay protocol version is unsupported` | `UNSUPPORTED_VERSION`: agent and relay releases differ. | Install the same release on both. |
| `relay rejected tunnel; reserve the name in the dashboard or omit --subdomain subdomain="myapp" code=SubdomainNotAllowed` | `SUBDOMAIN_NOT_ALLOWED`: the token's policy does not allow this name. Causes: a `temporary` token with `--subdomain`; a `reserved` token without `--subdomain` or with a name not in its allowlist; an `any` token with a name you have not reserved; a reservation still pending approval. | Check the policy table in [agents.md](agents.md#1-choose-a-bind-policy). Reserve the name (and get it approved), add it to a `reserved` token's allowlist by creating a new token, or drop `--subdomain`. |
| same line with `code=SubdomainTaken` | `SUBDOMAIN_TAKEN`: another user owns the reservation, or another live tunnel already uses the name (often a second copy of the same agent, or an old Pod still running). | Stop the other agent, or use another name. In Kubernetes use one replica and `strategy: Recreate`. |
| same line with `code=SubdomainInvalid` | `SUBDOMAIN_INVALID`: the name breaks the rules (3–63 of `a-z`, `0-9`, `-`, alphanumeric at both ends) or is a system name (`www`, `api`, `mail`, `smtp`, `ftp`, `admin`, `dash`, `dashboard`, `vorp`, `vorpd`). | Use a valid name. |
| `relay rejected tunnel: your account's tunnel limit is reached; close another tunnel or ask an admin to raise it` | `TUNNEL_LIMIT`: the account already has its quota of live tunnels (3 by default), across all its agents. | Close a tunnel in the dashboard (**Tunnels**) or stop another agent, or ask an admin to raise **Max tunnels**. |
| `relay closed tunnel permanently subdomain=myapp reason=Forced` | Someone closed the tunnel in the dashboard (or `POST /api/tunnels/myapp/close`). | Intended. A supervisor may restart the agent and reopen it; stop the service to keep it closed. |
| `relay rejected or permanently closed every tunnel` | Printed last, after one of the lines above, when no requested tunnel is left. | Fix the line above it. |

When an agent requests several names (`--subdomain a --subdomain b`), a
rejected name is skipped and the agent keeps running with the rest.

## Agent keeps reconnecting

These are `WARN vorp_agent::session: agent session disconnected error=...`
lines. The agent retries by itself (1 s, doubling to 60 s, randomized; after a
working session drops, a random 0–5 s wait).

| `error=` text | Cause | Fix |
| --- | --- | --- |
| `agent connection failed: dial relay: Connection refused (os error 111)` | Nothing listens on the relay's port 443, or a firewall rejects it. | Check `systemctl is-active vorp` on the relay and the cloud firewall. |
| `dial relay: Connection timed out (os error 110)` | Packets dropped on the way, or wrong IP in DNS. | `dig +short A vorp.example.com`; open TCP 443 inbound on the relay. |
| `TLS handshake: invalid peer certificate: UnknownIssuer` | The agent does not trust the relay's certificate: no CA bundle on the agent machine, or the relay uses a test/self-signed certificate. | Install CA certificates (`apk add ca-certificates`, `apt-get install ca-certificates`), or pass `--ca-cert` for a test relay. |
| `TLS handshake: invalid peer certificate: certificate not valid for name "relay.example.net"; ...` | `--relay-host` is a name the relay's certificate does not cover. | Use `vorp.<base domain>`. |
| `relay did not negotiate vorp-agent/1 ALPN`, or a TLS alert such as `NoApplicationProtocol` | Something other than vorp answers on 443: the Cloudflare proxy (orange cloud), nginx, or a load balancer that terminates TLS. | Set the DNS records to DNS only and point them at the relay. Nothing may terminate TLS in front of the relay. |
| `missed three heartbeats`, `relay closed yamux session`, `agent registration timed out` | The connection dropped or stalled, or the relay restarted. | None if it recovers. If it repeats, check the network path and the relay's log. |

## What visitors see

Status codes the relay itself returns. They have an empty body.

| Status | Cause | Fix |
| --- | --- | --- |
| `404` | No live tunnel has that name; or the host is not `<name>.<base domain>` (names with an extra dot, such as `a.b.example.com`, are never routed); or the agent is disconnected. | Check the agent's log for `tunnel registered`. A random name changes on every reconnect: use the current URL or a reserved name. |
| `502` | The agent could not reach the upstream. The agent logs `local upstream unavailable error=Connection refused (os error 111)`. Also when the agent's connection dropped during the request, or the upstream sent an invalid response. | Start the service, or fix `--upstream`. Test on the agent machine: `curl -sS http://127.0.0.1:3000/`. |
| `503` with `Retry-After: 1` | A concurrency limit is full: the user's concurrent-request quota (default 64; a request waits up to 10 s for a slot), the tunnel's limit (`--tunnel-requests`, 128), the relay's limit (`--max-requests`, 256), or the WebSocket limits (`--tunnel-websockets`, `--max-websockets`, 128 each). | Lower client concurrency, or raise the limit: per-user in the dashboard (**Admin**), the others as `vorp serve` flags. |
| `504` | The upstream sent no response headers within `--response-timeout-secs` (30 s; reset while the request body is still uploading). | Make the service answer sooner (send headers first for long work, or use SSE), or raise `--response-timeout-secs`. |
| `429` with `Retry-After: 1` | Over the per-IP request rate, `--rate-limit-rps` (200 per second, burst 400). Clients behind one NAT share it. | Slow the client or raise `--rate-limit-rps`. |
| `431` | The request head is larger than 64 KiB once encoded (very large cookies or headers). | Shrink the headers. |
| `400` | Ambiguous body framing: conflicting `Content-Length` values, a non-numeric length, or a `Transfer-Encoding` other than `chunked`. Also a header value that is not valid UTF-8. The relay logs `rejected ambiguous request framing`, or `HTTP/1 connection failed ... error=invalid content-length parsed` when the HTTP parser refused it first. | Fix the client. This guards against request smuggling and is not configurable. |
| Connection closed without a response | Over `--max-connections` (1024) or `--max-connections-per-ip` (64, agents and browsers counted together). | Raise the limits, and `LimitNOFILE` in the unit above `--max-connections`. |
| Response cut off mid-body | The upstream failed after it started answering; the relay does not pretend the body is complete. | Check the service. |
| Transfers slow down but do not fail | The user's bandwidth quota (default 10 MiB/s, uploads and downloads combined) pauses traffic instead of dropping it. | An admin raises the user's bandwidth limit. |

There is no `413`: the relay does not limit body size. Limit upload size in
the service if you need to.

WebSockets work over HTTP/1.1. The relay does not offer WebSockets over
HTTP/2.

## Relay fails to start

Read `journalctl -u vorp -n 50 --no-pager`. The cause follows `Caused by:`.

| Output | Cause | Fix |
| --- | --- | --- |
| `TLS setup failed: read certificate /etc/vorp/fullchain.pem: No such file or directory (os error 2)` | The Certbot deploy hook has not copied the certificate. | See [certificates.md](certificates.md#test-the-hook-by-hand). |
| `TLS setup failed: read certificate ...: Permission denied (os error 13)` | The `vorp` user cannot read the files. | `sudo stat -c '%U %G %a %n' /etc/vorp/*.pem` must show `root vorp 640`; rerun the hook. |
| `TLS setup failed: parse certificate: ...` or `parse private key: ...` | The file is not a PEM certificate chain or key. | Point `tls.cert` at `fullchain.pem` and `tls.key` at `privkey.pem`. |
| `relay listener failed: bind 0.0.0.0:443: Address already in use (os error 98)` | Another process holds TCP 443. | `sudo ss -ltnp 'sport = :443'` shows it. Stop it; the relay must own 443. |
| `relay listener failed: bind 0.0.0.0:443: Permission denied (os error 13)` | Started by hand as a normal user. Ports below 1024 need `CAP_NET_BIND_SERVICE`. | Use the systemd unit, which grants it, or test on a high port with `--listen`. |
| `repository failed: database operation failed: unable to open database file: /var/lib/vorp/vorp.sqlite3` | The directory is missing or not writable by `vorp`. | `sudo install -d -o vorp -g vorp -m 0750 /var/lib/vorp`. |
| `relay configuration is incomplete` / `tls: set both tls.cert and tls.key ...` | No certificate paths. | Set `tls.cert` and `tls.key` in `/etc/vorp/vorp.yaml` (or `--tls-cert` and `--tls-key`). |
| `relay configuration is incomplete` / `base_domain is not set: ...` | No base domain. | Set `base_domain` in `/etc/vorp/vorp.yaml` (or `--base-domain`). |
| `/etc/vorp/vorp.yaml:4: unknown key ...` or another message starting with the file name | A typo, a wrong type, or a secret key in the config file. | Fix that line, then `sudo -u vorp vorp config show --relay` to check before restarting. |
| `relay configuration invalid: development token requires self-signed TLS and a loopback listener` | `VORP_DEV_TOKEN` is set in the environment. | Unset it. It is only for local development. |

## Relay log lines while running

| Line | Meaning |
| --- | --- |
| `INFO relay listening address=0.0.0.0:443` | Started. |
| `INFO agent connected machine_id=... user_id=3` / `INFO agent disconnected ...` | An agent session started or ended. `machine_id` is per agent process. |
| `INFO tunnel registered subdomain=myapp user_id=3` | A tunnel went live. |
| `WARN agent session ended peer=203.0.113.50:41234 error=invalid agent handshake` | An agent was refused, usually a wrong or revoked token or a version mismatch. |
| `WARN TLS handshake failed peer=... error=...` | A client failed TLS. Internet scanners cause most of these; ignore unless your own agents or users fail. |
| `INFO TLS certificate reloaded` | New certificate files were picked up (checked every 30 s). |
| `WARN TLS certificate reload failed; retaining the last valid certificate` | The new files are unreadable or invalid. The old certificate keeps serving until it expires. Fix the files. |
| `WARN rejected ambiguous request framing` | A visitor got `400` (see above). |

## Dashboard and API

| Symptom | Cause | Fix |
| --- | --- | --- |
| `403 {"error":"forbidden"}` on a `POST`, `PUT` or `DELETE` | Missing `X-Vorp-Csrf: 1` header; or not an admin on an admin route; or `POST /api/signup` while signup is closed. | Send the header. Use an admin account. |
| `403 {"error":"password change required"}` | The account was created by an admin or reset on the host. | Change the password in the dashboard, or `POST /api/password`. |
| `401 {"error":"unauthorized"}` | No session, an expired one (sessions last 24 hours), or a wrong password at login. A password change ends all sessions. | Log in again. |
| `409 {"error":"conflict"}` from `/api/bootstrap` | An account already exists. | Log in instead. If nobody known created it, see [setup.md](setup.md#decision-points). |
| `400 {"error":"password must contain 12 to 1024 characters"}` | Password too short or too long. | Use 12 to 1024 characters. |
| `429 {"error":"rate limit exceeded"}` | Too many sign-in attempts: 10 per account or 120 in total per minute, or 4 already being checked. | Wait a minute. |
| `503 {"error":"relay disconnect unavailable"}` | The relay could not apply the change to live sessions (it is shutting down). | Retry. |
| The dashboard shows `404` | The request's host is neither `vorp.<base domain>` nor `<name>.<base domain>`. | Open `https://vorp.<base domain>`. |
| Lost admin password | | On the relay host: `sudo -u vorp vorp admin reset-password --email you@example.com --database-path /var/lib/vorp/vorp.sqlite3`. It prints `New password for you@example.com: ...` once and ends that account's sessions. |
