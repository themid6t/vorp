# vorp

`vorp` is a self-hosted reverse tunnel relay. The same Rust binary runs the
relay (`vorp serve`) and an outbound-only agent (`vorp`). A single TLS listener
uses ALPN to separate agent sessions from the dashboard and public tunnel
traffic. Tunnel bodies stream through yamux in bounded frames.

The [roadmap](docs/roadmap.md) records M0–M2 as done, M3 as partial, and the
staging portion of M4 as active. Today the binary provides tunnels, a
SQLite-backed dashboard, users, sessions, agent tokens, bind policies, a
per-user traffic feed, live token revocation, and admin-set per-user quotas
(tunnels, concurrent requests, bandwidth) that slow traffic before refusing it.
The dashboard's HTTP API is listed in [docs/api.md](docs/api.md).
Supplied certificate files are checked every 30 seconds and reloaded for new
TLS handshakes without interrupting existing connections; an invalid
replacement leaves the last valid certificate in use. ACME DNS-01 and renewal,
agent sidecar probes, `install-service`, and `doctor` are still to build. M4
container and installer artifacts remain. See the [wire contract](docs/protocol.md)
for protocol details.

## Run with an existing wildcard certificate

Point the root and wildcard DNS names at the relay, and provide a certificate
valid for both `example.com` and `*.example.com`:

```sh
vorp serve \
  --base-domain example.com \
  --tls-cert /path/to/fullchain.pem \
  --tls-key /path/to/privkey.pem \
  --database-path /var/lib/vorp/vorp.sqlite3
```

For Cloudflare DNS and Let's Encrypt, follow the current
[certificate setup and renewal procedure](docs/certificates.md). The relay
currently reads supplied files; its built-in ACME issuer remains planned.

The relay listens on `0.0.0.0:443` by default. Open `https://example.com/`
right away: on an empty database the dashboard asks you to **create the admin
account**, which works exactly once. Until you do, anyone who reaches the URL
could claim it. Signup defaults to `closed`, so the admin adds users, who must
choose their own password at first login; `--signup open` lets anyone
register. Users request reserved subdomains and an admin approves them, unless
the admin allows a user to reserve directly. Mint an agent token in the dashboard and copy it when shown; the raw
value cannot be retrieved later.

If an account's password is lost, reset it on the relay host as the user that
owns the database. The relay can keep running:

```sh
sudo -u vorp vorp admin reset-password --email you@example.com \
  --database-path /var/lib/vorp/vorp.sqlite3
```

It prints a new random password once and ends that account's sessions. The
dashboard asks for a new password at the next login.

Store the token locally by passing it on standard input, then start the agent:

```sh
vorp authtoken < /path/to/token.txt
vorp --relay-host example.com --upstream http://127.0.0.1:3000
```

The agent resolves `--relay-host` on port 443; `--relay-addr 203.0.113.10:443`
dials a fixed address instead while still verifying the certificate for the
relay host.

The token file defaults to `$XDG_CONFIG_HOME/vorp/authtoken`, or
`$HOME/.config/vorp/authtoken` where XDG is unset. `--token-file` chooses another
path; `VORP_TOKEN` can supply the token without a file. The agent rejects a
non-loopback upstream unless `--allow-remote-targets` is explicit.

For local development, `--listen 127.0.0.1:8443` with `VORP_DEV_TOKEN` lets the
relay run without the dashboard. Use `--dev-self-signed --dev-cert-out
./vorp-dev.crt` to create a temporary certificate, and point the agent's
`--ca-cert` at that file. Give the agent the same development token. The token
is accepted only when **both** self-signed TLS and a loopback listener are
configured. To test supplied certificate files instead, use the normal
SQLite-backed account and token flow.

A high-entropy tunnel URL prevents casual guessing; it is not access control.
Anyone holding the URL can reach the tunneled application. Protect sensitive
upstreams with their own authentication.

## CI and staging

Pull requests with non-Markdown changes run formatting, Clippy, all-feature
tests, and `cargo deny`. Markdown-only changes keep a lightweight green check
and skip the Rust gates and deployment. After the checks pass on `main`,
GitHub Actions builds a static Linux amd64 binary and deploys it to staging at
`https://vorp-staging.themidst.xyz/`. The deployment checks the binary hash,
restarts the service, verifies `/healthz`, and restores the previous binary
if it fails. The [release plan](docs/releases.md) records the agreed single
`main` branch and future tag-triggered production deployment. Production
automation is not configured yet.
