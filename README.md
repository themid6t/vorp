# vorp

`vorp` is a self-hosted reverse tunnel relay, like ngrok, that you run on your
own server. One Rust binary does both jobs: `vorp serve` runs the relay, and
`vorp` runs the agent next to the service you want to expose. The agent only
makes outbound connections, so it works behind NAT and firewalls.

- **One process, no external services.** The relay terminates TLS on `:443`
  and routes on ALPN: agent sessions on one side, the dashboard and public
  tunnels on the other. There is no nginx. State lives in an embedded SQLite
  database.
- **Bodies stream.** Request and response bodies pass through in bounded
  frames with backpressure, so large uploads, downloads, SSE and WebSockets
  work.
- **Multi-user.** The embedded dashboard has accounts, agent tokens, reserved
  names with admin approval, per-user quotas, live token revocation and a
  per-user traffic feed.

Status: v0.0.1, Linux amd64 and arm64, MIT licensed. Setup is still manual. One-command
setup with built-in ACME is planned for v0.1; see the [roadmap](docs/roadmap.md).

## Install

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
```

The installer checks the release signature and the archive checksum, then
installs `/usr/local/bin/vorp`. The same binary is the relay and the agent.

## Run a relay

You need a server with a public IP and TCP/443 open, a domain such as
`example.com` with DNS records for `example.com` and `*.example.com` pointing at
the server, and a wildcard certificate covering both names. With Cloudflare DNS
and Let's Encrypt, follow the [certificate procedure](docs/certificates.md).

```sh
vorp serve \
  --base-domain example.com \
  --tls-cert /etc/vorp/fullchain.pem \
  --tls-key /etc/vorp/privkey.pem \
  --database-path /var/lib/vorp/vorp.sqlite3
```

To serve the dashboard from a name other than the base domain, add
`--dashboard-host dashboard.example.com`. That name must also resolve to the
relay. [`deploy/vorp-staging.service`](deploy/vorp-staging.service) is a
hardened systemd unit you can adapt.

**Create the admin account right away.** On an empty database the dashboard
asks for one, and whoever gets there first gets it. Signup is `closed` by
default: the admin adds users, who choose their own password at first login.
`--signup open` lets anyone register. Users request reserved names and an
admin approves them, unless the admin lets a user reserve directly.

Supplied certificate files are re-read every 30 seconds and used for new
connections without dropping existing ones. An invalid replacement leaves the
last valid certificate in use.

To recover a lost password, run this on the relay host as the user that owns
the database. The relay can keep running:

```sh
sudo -u vorp vorp admin reset-password --email you@example.com \
  --database-path /var/lib/vorp/vorp.sqlite3
```

## Run an agent

Create an agent token in the dashboard and copy it; it is shown only once.
Save it, then start a tunnel:

```sh
vorp authtoken < token.txt
vorp --relay-host example.com --upstream http://127.0.0.1:3000
vorp --relay-host example.com --upstream http://127.0.0.1:3000 --subdomain myapp
```

Without `--subdomain`, the tunnel gets a random name. A reserved name needs a
token that allows it: `any` allows the user's reserved names, and `reserved`
allows only the names in the token's allowlist.

- The token is read from `$XDG_CONFIG_HOME/vorp/authtoken` (or
  `~/.config/vorp/authtoken`); `--token-file` or `VORP_TOKEN` override it.
- Upstreams other than loopback are rejected unless `--allow-remote-targets`
  is set, for example in a sidecar container pointing at `http://app:3000`.
- `--relay-addr 203.0.113.10:443` dials a fixed address while still checking
  the certificate for `--relay-host`.

**A tunnel URL is not access control.** Random names stop casual guessing, but
anyone holding the URL can reach the service. Protect sensitive upstreams with
their own authentication.

## Develop

The dashboard is a Svelte app in `crates/web/ui` that is embedded into the
binary, so build it before Cargo:

```sh
(cd crates/web/ui && npm ci && npm run build)
cargo build
```

For live reload, start a local relay and run `npm run dev` in `crates/web/ui`.
It proxies `/api` to `https://127.0.0.1:8443` (override with `VORP_RELAY`).

For a local relay without the dashboard, run `vorp serve` with `--listen
127.0.0.1:8443`, `--dev-self-signed --dev-cert-out ./vorp-dev.crt` and
`VORP_DEV_TOKEN`. Point the agent's `--ca-cert` at that file and give it the
same token. The relay accepts the development token only with **both** a
self-signed certificate and a loopback listener.

Contributor rules are in [AGENTS.md](AGENTS.md). The design is documented in:

- [Wire protocol](docs/protocol.md)
- [Dashboard API](docs/api.md)
- [SQLite schema](docs/schema.md)
- [CI and releases](docs/releases.md)
