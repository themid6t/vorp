# Relay setup runbook

This runbook sets up a vorp relay on a fresh Linux server. It is
written so that an AI agent can execute it step by step. A person can follow
it the same way. The [README](../README.md#set-up-a-relay) has the same
procedure in short form; this file adds the checks between the steps.

Rules for whoever runs it:

- Run every step in order. Each step ends with a check. Do not continue until
  the check shows the expected result.
- When a check fails, look for the matching entry in
  [Decision points](#decision-points) or in
  [troubleshooting.md](troubleshooting.md). Stop and ask the human when this
  file says so.
- Never print, log or commit the Cloudflare token, the TLS private key, an
  agent token or the admin password.

vorp does not have `vorp setup`, `vorp doctor` or built-in ACME yet. They are
planned.
Until then, the certificate comes from Certbot.

## Inputs to collect first

Ask the human for these before touching the server.

| Input | Example | Notes |
| --- | --- | --- |
| Base domain | `example.com` | Tunnels are served at `*.<base domain>`. It may be a subdomain, such as `tunnels.example.com`. |
| Server public IPv4 | `203.0.113.10` | From the cloud provider's console. |
| Cloudflare DNS for the zone | yes / no | The certificate is issued with a Cloudflare DNS challenge. Without Cloudflare, stop: see [Decision points](#decision-points). |
| Cloudflare API token | (secret) | Scoped to the zone with **Zone → DNS → Edit**. Not the Global API Key. |
| Admin email | `you@example.com` | Used for Let's Encrypt and for the first dashboard account. |
| Is the root name free? | yes / no | "Free" means nothing else (a website, mail web UI) uses `example.com` itself. |

From the last answer, pick the dashboard host:

- Root name free: the dashboard is on the base domain. `DASHBOARD=example.com`.
- Root name in use: put the dashboard on a name under the wildcard, such as
  `dashboard.example.com`. `DASHBOARD=dashboard.example.com`. The name
  `dashboard` can never be taken by a tunnel; it is on the relay's list of
  reserved names.

Every command block below uses these shell variables. Set them in each new
shell:

```sh
DOMAIN=example.com
DASHBOARD=example.com
SERVER_IP=203.0.113.10
EMAIL=you@example.com
```

## Preflight checks

Run these on the server.

### P1. Operating system and architecture

```sh
uname -sm
```

Expected: `Linux x86_64` or `Linux aarch64`. Anything else is not supported for a
relay.

### P2. Package manager and systemd

```sh
command -v apt-get systemctl
```

Expected: two paths, such as `/usr/bin/apt-get` and `/usr/bin/systemctl`.
This runbook uses `apt-get` for Certbot. On another distribution, see
[Decision points](#decision-points).

### P3. Port 443 is free

```sh
sudo ss -Hltn 'sport = :443'
```

Expected: no output. Any line means something already listens on TCP 443.

### P4. DNS points at this server, unproxied

Create the records in Cloudflare first (or ask the human to):

| Type | Name | Value | Proxy status |
| --- | --- | --- | --- |
| A | `example.com` | server IP | DNS only (grey cloud) |
| A | `*.example.com` | server IP | DNS only (grey cloud) |

If the dashboard is on `dashboard.example.com`, the root A record is not
needed here; leave the root's existing record alone. The wildcard covers the
dashboard name.

Check, from the server, against a public resolver:

```sh
command -v dig >/dev/null || sudo apt-get install -y dnsutils
dig +short A "$DASHBOARD" @1.1.1.1
dig +short A "vorp-check-$(date +%s).$DOMAIN" @1.1.1.1
dig +short AAAA "vorp-check-$(date +%s).$DOMAIN" @1.1.1.1
```

Expected: the first two commands print exactly `$SERVER_IP` and nothing else.
The third prints nothing.

- A Cloudflare address (for example `104.x` or `172.67.x`) means the record is
  proxied. Switch it to **DNS only**. The relay must terminate TLS itself;
  agents cannot connect through the Cloudflare proxy.
- An AAAA answer means IPv6 clients go elsewhere: the relay listens on IPv4
  (`0.0.0.0:443`) by default. Remove the AAAA record.
- DNS changes can take a few minutes. Re-run until the answers are right.

## Step 1. Install vorp

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
vorp --version
```

Expected: the installer ends with `vorp installed to /usr/local/bin/vorp`, and
`vorp --version` prints the release, such as `vorp 0.0.2`. The installer checks the release
signature and the archive's checksum and stops on any mismatch. It installs
`curl`, `gpg`, `jq` and the other tools it needs if they are missing.

## Step 2. Create the service user and directories

```sh
sudo useradd --system --home-dir /var/lib/vorp --shell /usr/sbin/nologin vorp
sudo install -d -o vorp -g vorp -m 0750 /var/lib/vorp
sudo install -d -o root -g vorp -m 0750 /etc/vorp
```

Check:

```sh
id -nG vorp
sudo stat -c '%U %G %a %n' /var/lib/vorp /etc/vorp
```

Expected:

```
vorp
vorp vorp 750 /var/lib/vorp
root vorp 750 /etc/vorp
```

## Step 3. Install Certbot and store the Cloudflare token

```sh
sudo apt-get update
sudo apt-get install -y certbot python3-certbot-dns-cloudflare sqlite3
sudo install -d -m 0755 /etc/letsencrypt
sudo install -m 0600 /dev/null /etc/letsencrypt/cloudflare.ini
sudoedit /etc/letsencrypt/cloudflare.ini
```

The file holds one line:

```ini
dns_cloudflare_api_token = YOUR_ZONE_SCOPED_TOKEN
```

The human should type the token into `sudoedit` themselves. If an agent must
write it, it must not put the token in a command line, shell history or a log.

Check the file and the token. The token goes to curl on standard input, so it
never appears in the process list. The request lists the zones the token can
see, which is the same lookup Certbot's plugin does:

```sh
sudo stat -c '%U %G %a' /etc/letsencrypt/cloudflare.ini
sudo sh -c 'printf "Authorization: Bearer %s\n" "$(sed -n "s/^dns_cloudflare_api_token *= *//p" /etc/letsencrypt/cloudflare.ini)" \
  | curl -fsS -H @- https://api.cloudflare.com/client/v4/zones' | jq -r '.success, .result[].name'
```

Expected: `root root 600`, then `true` and a list of zone names that includes
the zone holding your domain (`example.com`, also for
`tunnels.example.com`). `curl: (22) ... error: 400` or `403`, or a list
without your zone, means the token is wrong, expired or scoped to another
zone; ask the human for a new one.

## Step 4. Install the renewal hook and get the certificate

```sh
sudo install -d /usr/local/libexec
sudo curl -fsSL -o /usr/local/libexec/vorp-cert-deploy \
  https://raw.githubusercontent.com/themid6t/vorp/main/deploy/cert-deploy.sh
sudo chmod 0755 /usr/local/libexec/vorp-cert-deploy

sudo certbot certonly --non-interactive --agree-tos --email "$EMAIL" \
  --dns-cloudflare --dns-cloudflare-credentials /etc/letsencrypt/cloudflare.ini \
  --dns-cloudflare-propagation-seconds 30 --key-type ecdsa \
  --cert-name "$DOMAIN" --deploy-hook /usr/local/libexec/vorp-cert-deploy \
  -d "$DOMAIN" -d "*.$DOMAIN"
```

The hook copies `fullchain.pem` and `privkey.pem` into `/etc/vorp` as
`root:vorp 0640`, at first issuance and after every renewal.
[certificates.md](certificates.md) explains each part.

Check:

```sh
sudo stat -c '%U %G %a %n' /etc/vorp/fullchain.pem /etc/vorp/privkey.pem
sudo openssl x509 -in /etc/vorp/fullchain.pem -noout -ext subjectAltName
systemctl is-enabled certbot.timer
```

Expected:

```
root vorp 640 /etc/vorp/fullchain.pem
root vorp 640 /etc/vorp/privkey.pem
X509v3 Subject Alternative Name:
    DNS:*.example.com, DNS:example.com
enabled
```

The two DNS names may appear in either order. If `/etc/vorp/*.pem` is
missing but Certbot succeeded, the hook did not run: check
`ls -l /usr/local/libexec/vorp-cert-deploy` and rerun the hook by hand as shown
in [certificates.md](certificates.md#test-the-hook-by-hand).

## Step 5. Write the config file and install the systemd unit

The relay reads its settings from `/etc/vorp/vorp.yaml`. The file holds no
secrets: the TLS key is only a path, and agent tokens never go in it.

```sh
curl -fsSL https://raw.githubusercontent.com/themid6t/vorp/main/deploy/vorp.yaml \
  | sed "s/example\.com/$DOMAIN/g" \
  | sudo tee /etc/vorp/vorp.yaml >/dev/null
sudo chmod 0644 /etc/vorp/vorp.yaml
```

If the dashboard is not on the base domain, set `dashboard_host`:

```sh
[ "$DASHBOARD" = "$DOMAIN" ] || sudo sed -i \
  "s|^# dashboard_host: .*|dashboard_host: $DASHBOARD|" /etc/vorp/vorp.yaml
```

Every key is described in the file's comments. Leave `signup` closed for now.

Install the unit. Its `ExecStart=` only names the config file:

```sh
curl -fsSL https://raw.githubusercontent.com/themid6t/vorp/main/deploy/vorp.service \
  | sudo tee /etc/systemd/system/vorp.service >/dev/null
```

The unit sets `NO_COLOR=1`, so journal lines are plain text and easy to
`grep`.

Do **not** start the service yet. Step 6 creates the admin account first.

Check:

```sh
sudo systemctl daemon-reload
grep '^ExecStart=' /etc/systemd/system/vorp.service
sudo -u vorp vorp config show --relay
```

Expected: the unit line, then the effective settings, each followed by where
it came from (`file` or `default`). With your names:

```
ExecStart=/usr/local/bin/vorp serve --config /etc/vorp/vorp.yaml
config                         /etc/vorp/vorp.yaml  (default)
base_domain                    example.com  (file)
dashboard_host                 example.com  (default)
listen                         0.0.0.0:443  (default)
database_path                  /var/lib/vorp/vorp.sqlite3  (file)
signup                         closed  (default)
tls.cert                       /etc/vorp/fullchain.pem  (file)
tls.key                        /etc/vorp/privkey.pem  (file)
limits.max_connections         1024  (default)
...
```

With a separate dashboard host, `dashboard_host` shows
`dashboard.example.com  (file)`. A typo in a key stops `vorp config show` and
the relay with the file name and line, such as
``/etc/vorp/vorp.yaml:4: unknown key `relay_hots` ``. Fix the line and rerun the
check.

## Step 6. Create the admin account before going public

On a fresh database, the first account created becomes the admin, and
`POST /api/bootstrap` is open to anyone until then. If the relay starts on the
public port first, a stranger who finds it can take the admin account. Close
that race by creating the admin while the relay listens only on loopback.

Start a temporary relay on `127.0.0.1:8443` with the same config. A flag
overrides the file, so `--listen` moves it to loopback:

```sh
sudo systemd-run --unit vorp-bootstrap -p User=vorp -p Group=vorp \
  /usr/local/bin/vorp serve --config /etc/vorp/vorp.yaml --listen 127.0.0.1:8443
sleep 2
curl -fsS --resolve "$DASHBOARD:8443:127.0.0.1" "https://$DASHBOARD:8443/api/config"; echo
```

Expected:

```
{"signup_mode":"closed","needs_bootstrap":true,"base_domain":"example.com"}
```

If `needs_bootstrap` is `false`, an account already exists in this database.
Stop and ask the human (see [Decision points](#decision-points)).

Create the admin. The password is random, 32 hex characters (the minimum is
12 characters). The JSON body goes to curl on standard input, so the password
is not in the process list:

```sh
ADMIN_PASSWORD=$(openssl rand -hex 16)
printf '{"email":"%s","password":"%s"}' "$EMAIL" "$ADMIN_PASSWORD" \
  | curl -sS --resolve "$DASHBOARD:8443:127.0.0.1" \
      -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' --data-binary @- \
      -w ' %{http_code}\n' "https://$DASHBOARD:8443/api/bootstrap"
```

Expected: `{"ok":true} 200`. Other answers:

- `{"error":"conflict"} 409`: an account already exists. Stop and ask.
- `{"error":"forbidden"} 403`: the `X-Vorp-Csrf: 1` header is missing.
- `{"error":"email"} 400`: the email has no `@`.

Stop the temporary relay and confirm:

```sh
sudo systemctl stop vorp-bootstrap
sudo -u vorp sqlite3 /var/lib/vorp/vorp.sqlite3 'SELECT email, is_admin FROM users;'
```

Expected: `you@example.com|1`, and nothing else.

Keep `ADMIN_PASSWORD` for step 8 and for the report to the human. Give it to
the human once, then forget it. Do not write it to a file, a log or a commit.
The human can change it under **Account** in the dashboard.

## Step 7. Start the relay

```sh
sudo systemctl enable --now vorp
sleep 2
systemctl is-active vorp
curl -fsS -o /dev/null -w '%{http_code}\n' "https://$DASHBOARD/healthz"
curl -fsS "https://$DASHBOARD/api/config"; echo
journalctl -u vorp -n 20 --no-pager | grep -c 'relay listening'
```

Expected:

```
active
200
{"signup_mode":"closed","needs_bootstrap":false,"base_domain":"example.com"}
1
```

The `healthz` request also proves the certificate is valid for the dashboard
host, because curl verifies it. If the server cannot reach its own public IP
(some networks do not allow that), add
`--resolve "$DASHBOARD:443:127.0.0.1"` to the two curl commands, and run the
external check in step 9 instead.

## Step 8. Test a tunnel end to end

This runs a throwaway web server and agent on the relay host. It creates a
`temporary` token through the API, opens a tunnel, fetches a page through it,
then revokes the token.

Run the whole step in one shell session; it keeps process IDs in variables.

```sh
E=$(mktemp -d)
echo vorp-e2e-ok > "$E/index.html"
python3 -m http.server 18080 --bind 127.0.0.1 --directory "$E" >/dev/null 2>&1 &
HTTP_PID=$!
API="https://$DASHBOARD/api"
R="--resolve $DASHBOARD:443:127.0.0.1"

printf '{"email":"%s","password":"%s"}' "$EMAIL" "$ADMIN_PASSWORD" \
  | curl -fsS $R -c "$E/cookies" -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' \
      --data-binary @- "$API/login"; echo
curl -fsS $R -b "$E/cookies" -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' \
  -d '{"bind_policy":"temporary"}' "$API/tokens" > "$E/mint.json"
TOKEN_ID=$(jq -r .token.id "$E/mint.json")
jq -r .raw_token "$E/mint.json" | vorp authtoken --token-file "$E/token"
rm "$E/mint.json"

NO_COLOR=1 vorp --relay-host "$DOMAIN" --relay-addr 127.0.0.1:443 \
  --token-file "$E/token" --upstream http://127.0.0.1:18080 > "$E/agent.log" 2>&1 &
AGENT_PID=$!
sleep 3
URL=$(grep -o "https://[a-z2-7]*\.$DOMAIN" "$E/agent.log" | head -1); echo "$URL"
curl -fsS --resolve "${URL#https://}:443:127.0.0.1" "$URL/"
```

Expected: `{"ok":true}` from the login, an `agent token stored` log line, a
URL such as `https://k3n4xq7p2wd9a5bm.example.com`, then `vorp-e2e-ok`.

Clean up. Revoking the token disconnects the test agent; it exits with
`agent authentication failed`, at once or after one reconnect attempt (up to
5 seconds).

```sh
curl -fsS $R -b "$E/cookies" -X POST -H 'X-Vorp-Csrf: 1' "$API/tokens/$TOKEN_ID/revoke"; echo
curl -fsS $R -b "$E/cookies" -X POST -H 'X-Vorp-Csrf: 1' "$API/logout"; echo
sleep 5
tail -n 2 "$E/agent.log"
kill "$HTTP_PID" "$AGENT_PID" 2>/dev/null
rm -rf "$E"
```

Expected: `{"ok":true}` twice, then `Caused by:` and
`    agent authentication failed` as the last two log lines.

## Step 9. Post-setup checks

From a machine **outside** the server's network (the human's laptop, or the
agent's own machine):

```sh
curl -fsS -o /dev/null -w '%{http_code}\n' https://example.com/healthz
```

Expected: `200`. A timeout means TCP 443 is blocked by a cloud firewall or
security group. Open it.

On the server:

```sh
sudo certbot renew --dry-run 2>&1 | tail -3
systemctl list-timers certbot.timer --no-pager | grep -c certbot.timer
systemctl is-enabled vorp
```

Expected: the dry run ends with a line saying the renewals succeeded, the
timer count is `1`, and `enabled`.

## Report to the human

When every check passed, tell the human:

- The dashboard URL: `https://<DASHBOARD>`.
- The admin email and the generated password, once. Ask them to sign in and
  change it under **Account**.
- Signup is closed. The admin creates accounts under **Admin**; each new user
  must choose a new password at first login. `signup: open` in
  `/etc/vorp/vorp.yaml` lets anyone register.
- Next: [agents.md](agents.md) to connect services.
- A tunnel URL is not access control. Anyone with the URL can reach the
  service behind it.

## Decision points

Stop and ask the human in these cases. Do not work around them.

| Situation | Why it matters | Ask |
| --- | --- | --- |
| The root name already serves something (website, other proxy) | The relay would take over the root name's HTTPS. | Confirm the dashboard goes on `dashboard.<domain>` (or another name under the wildcard), or pick a different base domain such as `tunnels.example.com`. |
| The domain's DNS is not on Cloudflare | Step 3 and 4 use the Cloudflare DNS plugin; vorp has no built-in ACME yet. | Move the zone to Cloudflare, delegate a subdomain to Cloudflare, or supply a wildcard certificate another way (any PEM chain and key in `/etc/vorp` works). |
| No API token, or the token check fails | Certbot cannot create the challenge record. | Ask for a zone-scoped token with Zone → DNS → Edit. |
| Port 443 is in use (P3) | The relay must own TCP 443 and terminate TLS itself. It cannot sit behind nginx or a TLS-terminating proxy. | Ask whether to stop that service, or to use a different server. |
| DNS shows a Cloudflare IP or another IP (P4) | Proxied or wrong records break agents. | Ask the human to set both records to DNS only, pointing at this server. |
| `needs_bootstrap` is `false` before step 6, or bootstrap returns `409` | Someone already created an account, possibly not the human. | Ask before deleting `/var/lib/vorp/vorp.sqlite3*` and starting over. Never delete the database without approval. |
| Not Linux amd64/arm64, no systemd, or no `apt-get` | A relay runs on Linux amd64 or arm64; the steps assume systemd and apt. | Ask whether to adapt the Certbot install for this distribution or use another server. |
| The server cannot reach Let's Encrypt or the Cloudflare API | Issuance and renewal need outbound HTTPS. | Ask for outbound access to be opened. |

## Upgrading a relay installed with the flag-only unit

Relays set up before the config file existed run `vorp serve` with every
setting as a flag in `ExecStart=`. That unit keeps working unchanged with
newer releases: with no `/etc/vorp/vorp.yaml`, the relay uses its flags and
built-in defaults exactly as before. Moving to the config file is optional.

To move, write each flag as the matching key (`--base-domain` is
`base_domain`, `--tls-cert` is `tls.cert`, `--max-connections` is
`limits.max_connections`; [deploy/vorp.yaml](../deploy/vorp.yaml) lists them
all), then shorten the unit:

```sh
grep '^ExecStart=' /etc/systemd/system/vorp.service
sudoedit /etc/vorp/vorp.yaml
sudo -u vorp vorp config show --relay
sudo sed -i 's|^ExecStart=.*|ExecStart=/usr/local/bin/vorp serve --config /etc/vorp/vorp.yaml|' \
  /etc/systemd/system/vorp.service
sudo systemctl daemon-reload && sudo systemctl restart vorp
systemctl is-active vorp
```

Expected: `vorp config show --relay` shows the same values the old flags set,
each marked `(file)`, before the unit is changed; then `active`. A flag still
wins over the file, so a flag left in `ExecStart=` overrides the file's value.
