# vorp

Self-hosted reverse tunnels, like ngrok, on a server you control.

Run the relay on any VM with a public IP. Run the agent next to a local
service, and it gets a public HTTPS URL such as `https://myapp.example.com`.
The agent only makes outbound connections, so it works behind NAT, home
routers, corporate firewalls and Kubernetes clusters.

- **One binary, one process.** `vorp serve` is the relay, plain `vorp` is the
  agent. The relay terminates TLS itself on `:443`. It needs no nginx, no
  database server and no external services; its state lives in an embedded
  SQLite file.
- **Streaming end to end.** Request and response bodies stream through the
  tunnel with backpressure, so large uploads and downloads, server-sent events
  and WebSockets work.
- **Multi-user.** A built-in dashboard covers accounts, agent tokens, reserved
  names with admin approval, per-user quotas, live token revocation and a
  per-user traffic view.

```
 browser ──HTTPS──▶ relay :443 ◀──TLS (outbound)── agent ──HTTP──▶ your service
                    *.example.com                  any network
```

> **Status: v0.0.3.** The agent runs on Linux and macOS (amd64 and arm64)
> and Windows (amd64); the relay runs on Linux. Setting up a relay takes about
> fifteen minutes by hand; a one-command `vorp setup` with built-in
> certificates is planned for v0.1.

## Contents

- [Set up a relay](#set-up-a-relay)
- [Connect an agent](#connect-an-agent)
- [Operate the relay](#operate-the-relay)
- [Security notes](#security-notes)
- [Command reference](#command-reference)
- [Develop](#develop)

Detailed guides, written so an AI agent can follow them step by step:
[relay setup runbook](docs/setup.md), [connecting services](docs/agents.md)
and [troubleshooting](docs/troubleshooting.md). [llms.txt](llms.txt) indexes
them.

## Set up a relay

### 1. What you need

- A Linux VM (amd64 or arm64) with a public IP. 512 MB of RAM is plenty.
- Inbound **TCP 443** open. Port 80 is not needed.
- A domain whose DNS you can edit. These steps use Cloudflare DNS, because the
  certificate is issued through a DNS challenge.

These steps serve tunnels at `*.example.com`. The relay can also live under a
subdomain, such as `*.tunnels.example.com`; substitute it throughout.

### 2. Point DNS at the server

Create two records pointing at the server's public IP:

| Type | Name | Value |
| --- | --- | --- |
| A | `example.com` | server IP |
| A | `*.example.com` | server IP |

In Cloudflare, set both to **DNS only** (grey cloud). The Cloudflare proxy
would intercept TLS and break agent connections.

If the root name already serves something else, such as your website, leave it
alone and put the dashboard on a name under the wildcard instead, such as
`dashboard.example.com` (step 5).

### 3. Install vorp

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
vorp --version
```

The installer checks the release's signature and the archive's checksum, then
installs `/usr/local/bin/vorp`.

### 4. Get a wildcard certificate

Create a service user and its directories, then get a Let's Encrypt
certificate for `example.com` and `*.example.com` with Certbot's Cloudflare
plugin:

```sh
sudo useradd --system --home-dir /var/lib/vorp --shell /usr/sbin/nologin vorp
sudo install -d -o vorp -g vorp -m 0750 /var/lib/vorp
sudo install -d -o root -g vorp -m 0750 /etc/vorp

sudo apt-get update
sudo apt-get install -y certbot python3-certbot-dns-cloudflare sqlite3
sudo install -d -m 0755 /etc/letsencrypt
sudo install -m 0600 /dev/null /etc/letsencrypt/cloudflare.ini
sudoedit /etc/letsencrypt/cloudflare.ini
#   dns_cloudflare_api_token = <token with Zone → DNS → Edit on your zone>

sudo install -d /usr/local/libexec
sudo curl -fsSL -o /usr/local/libexec/vorp-cert-deploy \
  https://raw.githubusercontent.com/themid6t/vorp/main/deploy/cert-deploy.sh
sudo chmod 0755 /usr/local/libexec/vorp-cert-deploy

sudo certbot certonly --non-interactive --agree-tos --email you@example.com \
  --dns-cloudflare --dns-cloudflare-credentials /etc/letsencrypt/cloudflare.ini \
  --dns-cloudflare-propagation-seconds 30 --key-type ecdsa \
  --cert-name example.com --deploy-hook /usr/local/libexec/vorp-cert-deploy \
  -d example.com -d '*.example.com'
```

The deploy hook copies the certificate to `/etc/vorp`, where the `vorp` user
can read it, at first issuance and after every renewal. The relay reloads it
by itself within 30 seconds. Certbot's timer renews it automatically. The
[certificate guide](docs/certificates.md) explains each step and how to check
renewal.

### 5. Run it as a service

The relay reads its settings from `/etc/vorp/vorp.yaml`. Replace `YOUR-DOMAIN`
with your base domain:

```sh
curl -fsSL https://raw.githubusercontent.com/themid6t/vorp/main/deploy/vorp.yaml \
  | sed 's/example\.com/YOUR-DOMAIN/g' \
  | sudo tee /etc/vorp/vorp.yaml >/dev/null
curl -fsSL https://raw.githubusercontent.com/themid6t/vorp/main/deploy/vorp.service \
  | sudo tee /etc/systemd/system/vorp.service >/dev/null
sudo -u vorp vorp config show --relay   # check the effective settings
sudo systemctl daemon-reload
sudo systemctl enable --now vorp
curl -fsS https://YOUR-DOMAIN/healthz
```

[`deploy/vorp.service`](deploy/vorp.service) runs
`vorp serve --config /etc/vorp/vorp.yaml` as the `vorp` user with a hardened
sandbox. [`deploy/vorp.yaml`](deploy/vorp.yaml) describes every setting; to
put the dashboard on its own name, set `dashboard_host: dashboard.example.com`
there. After editing the file, run `sudo systemctl restart vorp`.

A relay installed earlier with every setting in the unit's `ExecStart=` keeps
working unchanged; moving to the config file is optional
([setup.md](docs/setup.md#upgrading-a-relay-installed-with-the-flag-only-unit)).

### 6. Create the admin account, right away

Open `https://example.com` (or your dashboard host). On a fresh install the
dashboard asks you to create the first account, which becomes the admin.
**Whoever opens it first gets it,** so do this as soon as the service starts.
To avoid that window entirely, create the admin through a loopback-only relay
before starting the service, as in
[setup.md step 6](docs/setup.md#step-6-create-the-admin-account-before-going-public).

Signup is closed by default: the admin creates accounts in the dashboard, and
each new user picks their own password at first login. Set `signup: open` in
`/etc/vorp/vorp.yaml` to let anyone register.

## Connect an agent

### 1. Create a token

In the dashboard, open **Tokens** and create one. Copy it straight away: it is
shown only once. Pick a bind policy:

| Policy | The agent may open |
| --- | --- |
| `any` | random names, and any name you have reserved |
| `temporary` | random names only |
| `reserved` | only the reserved names you list in the token's allowlist |

To use a fixed name such as `myapp`, reserve it under **Subdomains** first. An admin
approves reservations unless your account may reserve directly.

### 2. Install and log in

On the machine running the service (Linux or macOS):

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
vorp login example.com     # paste the token at the hidden prompt
```

On macOS the installer needs GnuPG and jq to verify the download:
`brew install gnupg jq` first.

On Windows there is no installer yet. Download `vorp_<version>_windows_amd64.zip`
and `SHA256SUMS` from the [latest release](https://github.com/themid6t/vorp/releases/latest),
check the hash, and put `vorp.exe` somewhere on your `PATH`. In PowerShell:

```powershell
(Get-FileHash .\vorp_0.0.3_windows_amd64.zip -Algorithm SHA256).Hash.ToLower()
Select-String windows_amd64 .\SHA256SUMS      # the two hashes must match
Expand-Archive .\vorp_0.0.3_windows_amd64.zip -DestinationPath "$env:LOCALAPPDATA\vorp"
Get-Content .\token.txt | & "$env:LOCALAPPDATA\vorp\vorp.exe" login example.com
```

`vorp login` saves the token to `~/.config/vorp/authtoken` (on Windows
`%APPDATA%\vorp\authtoken`), readable only by you, and writes
`relay_host: example.com` to `config.yaml` beside it. The relay name is any
name that resolves to the relay, such as the base domain or the dashboard
host. The token is never an argument, where `ps` and shell history would see
it, and never goes in the config file.

### 3. Open a tunnel

```sh
# A random name, such as https://k3n4xq7p2wd9a5bm.example.com
vorp --upstream http://127.0.0.1:3000

# A reserved name: https://myapp.example.com
vorp --upstream http://127.0.0.1:3000 --subdomain myapp
```

The agent logs the public URL once the tunnel is up, and reconnects with
backoff if the connection drops. `vorp config show` prints the settings in
effect and where each came from.

### 4. Keep it running

As a systemd user service:

```ini
# ~/.config/systemd/user/vorp-myapp.service
[Unit]
Description=vorp tunnel for myapp
After=network-online.target

[Service]
ExecStart=/usr/local/bin/vorp --upstream http://127.0.0.1:3000 --subdomain myapp
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now vorp-myapp
sudo loginctl enable-linger "$USER"   # keep it running after you log out
```

On macOS, use a launchd agent instead; [docs/agents.md](docs/agents.md#launchd-agent-macos)
has one.

### In a container or Kubernetes

In a Kubernetes Pod sidecar the app is on `127.0.0.1`, so no extra flag is
needed. When the agent runs in its own container and reaches the app by a
service name, such as a Compose service, add `--allow-remote-targets`, and
mount the token as a file:

```sh
vorp --relay-host example.com --token-file /etc/vorp/token \
  --upstream http://app:3000 --allow-remote-targets --subdomain myapp
```

The binary is static, so any small image with CA certificates works, such as
`alpine` with `ca-certificates`. [docs/agents.md](docs/agents.md) has a
Dockerfile, a Deployment and pm2 instructions.

## Operate the relay

- **Upgrade:** rerun the installer, then `sudo systemctl restart vorp`. The
  previous binary is kept as `/usr/local/bin/vorp.rollback`. Agents reconnect
  by themselves.
- **Back up** the database while the relay runs:

  ```sh
  sudo -u vorp sqlite3 /var/lib/vorp/vorp.sqlite3 ".backup /var/lib/vorp/backup.sqlite3"
  ```

  It holds accounts, token hashes, reservations and quotas, and nothing about
  live tunnels.
- **Logs:** `journalctl -u vorp -f`.
- **Lost password:** reset it on the host. The relay can keep running:

  ```sh
  sudo -u vorp vorp admin reset-password --email you@example.com \
    --database-path /var/lib/vorp/vorp.sqlite3
  ```

  It prints a new password once and signs that account out everywhere.
- **Quotas:** non-admin users may hold 3 live tunnels by default. Admins change
  the defaults and per-user limits (tunnels, concurrent requests, bandwidth) in
  the dashboard. A user over the bandwidth limit is slowed down before being
  refused.

## Security notes

- **A tunnel URL is not access control.** Random names stop casual guessing,
  but anyone who has the URL can reach your service. Put authentication in
  front of anything sensitive.
- The agent refuses upstreams other than loopback unless
  `--allow-remote-targets` is set, so a tunnel cannot reach other machines on
  the agent's network by accident.
- Revoking a token in the dashboard disconnects agents already using it.
- Agent tokens are stored only as hashes and shown once. Passwords use
  argon2id.
- The relay rejects ambiguous request framing (request smuggling), strips
  hop-by-hop headers, and sets `X-Forwarded-For`, `X-Forwarded-Host` and
  `X-Forwarded-Proto` itself instead of trusting the client's values.
- Keep the relay's DNS records unproxied. The relay sees client IPs directly
  and does not trust forwarding headers from a proxy in front of it.

## Command reference

Every setting below can be given three ways, with the same name: a flag
(`--max-connections`), a `VORP_*` environment variable, and a key in the
config file. A nested key's variable joins its path with `_`
(`limits.max_connections` is `VORP_LIMITS_MAX_CONNECTIONS`). For each
setting the flag wins, then the environment, then the file, then the default.
Unknown keys and secrets in the file are errors that name the file and line.

### `vorp serve` (relay)

Config file: `/etc/vorp/vorp.yaml`, or `--config PATH` / `VORP_CONFIG`. See
[deploy/vorp.yaml](deploy/vorp.yaml).

| Flag | Config key | Default | |
| --- | --- | --- | --- |
| `--base-domain` | `base_domain` | required | Tunnels are served at `*.<base-domain>` |
| `--dashboard-host` | `dashboard_host` | the base domain | Host name that serves the dashboard and `/api` |
| `--tls-cert`, `--tls-key` | `tls.cert`, `tls.key` | required | PEM certificate chain and key, re-read every 30 s |
| `--database-path` | `database_path` | `vorp.sqlite3` | SQLite file |
| `--listen` | `listen` | `0.0.0.0:443` | Listen address |
| `--signup` | `signup` | `closed` | `open` lets anyone create an account |
| `--max-connections` | `limits.max_connections` | 1024 | Concurrent TLS connections |
| `--max-connections-per-ip` | `limits.max_connections_per_ip` | 64 | Concurrent TLS connections from one IP |
| `--rate-limit-rps` | `limits.rate_limit_rps` | 200 | HTTP requests per second per IP (burst is twice this) |
| `--max-requests` / `--tunnel-requests` | `limits.max_requests` / `limits.tunnel_requests` | 256 / 128 | Concurrent proxied requests, overall / per tunnel |
| `--max-websockets` / `--tunnel-websockets` | `limits.max_websockets` / `limits.tunnel_websockets` | 128 / 128 | Concurrent WebSockets, overall / per tunnel |
| `--response-timeout-secs` | `limits.response_timeout_secs` | 30 | Wait for an upstream's response headers |

The development flags `--dev-self-signed`, `--dev-cert-out` and `--dev-token`
are flags only, so a config file can never turn them on.

### `vorp` (agent)

Config file: `~/.config/vorp/config.yaml` (`%APPDATA%\vorp\config.yaml` on
Windows), or `--config PATH` / `VORP_CONFIG`.

| Flag | Config key | |
| --- | --- | --- |
| `--relay-host` | `relay_host` | Relay name to connect to; its certificate is checked. Default `localhost` |
| `--upstream` | `upstream` | Local service URL, such as `http://127.0.0.1:3000` |
| `--subdomain` | `subdomains` | Reserved name to use, repeatable (`VORP_SUBDOMAINS` is comma-separated); omit for a random name |
| `--token-file` | `token_file` | Token file; defaults to `authtoken` next to the config file. `--token` / `VORP_TOKEN` also work |
| `--allow-remote-targets` | `allow_remote_targets` | Allow an upstream that is not loopback |
| `--relay-addr` | `relay_addr` | Dial this `IP:port` instead of resolving the relay host |
| `--ca-cert` | `ca_cert` | Extra CA to trust, for development certificates |

Other commands:

- `vorp login HOST` saves a token (hidden prompt, or standard input) and sets
  `relay_host` in the agent config. It edits only that line, and refuses to
  replace a different relay without `--force`.
- `vorp authtoken` saves a token from standard input.
- `vorp config path [--relay]` prints the config file in use.
- `vorp config show [--relay]` prints the effective settings and whether each
  came from a flag, the environment, the file or the default. It never prints
  a secret.
- `vorp admin reset-password` resets an account's password on the relay host.

## Develop

The dashboard is a Svelte app in `crates/web/ui`, embedded in the binary.
Build it before Cargo:

```sh
(cd crates/web/ui && npm ci && npm run build)
cargo build
```

To try a tunnel locally without a real certificate, start a relay with a
self-signed certificate and a development token, then an agent with the same
token. The relay accepts that token only with **both** a self-signed
certificate and a loopback listener:

```sh
VORP_DEV_TOKEN=dev-secret cargo run -- serve --base-domain localhost \
  --listen 127.0.0.1:8443 --dev-self-signed --dev-cert-out ./vorp-dev.crt

VORP_TOKEN=dev-secret cargo run -- --relay-host localhost \
  --relay-addr 127.0.0.1:8443 --ca-cert ./vorp-dev.crt \
  --upstream http://127.0.0.1:3000

# Use the name the agent logs. -k because curl rejects the dev certificate's
# *.localhost wildcard.
curl -k --resolve NAME.localhost:8443:127.0.0.1 https://NAME.localhost:8443/
```

For dashboard work with live reload, run `npm run dev` in `crates/web/ui`. It
proxies `/api` to `https://127.0.0.1:8443` (override with `VORP_RELAY`).

Contributor rules are in [AGENTS.md](AGENTS.md). Design documents:
[wire protocol](docs/protocol.md), [dashboard API](docs/api.md),
[SQLite schema](docs/schema.md), [CI and releases](docs/releases.md).

Guides: [setup runbook](docs/setup.md), [connecting services](docs/agents.md),
[troubleshooting](docs/troubleshooting.md), [certificates](docs/certificates.md).
AI agents can start from [llms.txt](llms.txt).

## License

[MIT](LICENSE)
