# TLS certificates with Cloudflare DNS and Certbot

Vorp currently needs a supplied certificate and private key in `serve` mode.
The built-in ACME DNS-01 issuer in the roadmap is not implemented yet. This
procedure matches the staging host as of October 2026. It uses Let's Encrypt,
Certbot, and the Cloudflare DNS plugin; Vorp itself still terminates TLS on
port 443. No nginx or Cloudflare proxy sits in the agent connection path.

## 1. DNS and Cloudflare token

For a relay at `vorp-staging.themidst.xyz`, make **DNS-only** Cloudflare A
records for `vorp-staging` and `*.vorp-staging`, both pointing at the relay's
public address. For another installation, substitute its relay domain and
create records for both that domain and its wildcard. DNS-only is necessary
for the agent's `vorp-agent/1` TLS ALPN connection to reach Vorp directly.
Open TCP/443 on the server. DNS-01 issuance itself does not require inbound
HTTP traffic.

Create a Cloudflare API token scoped to the DNS zone (`themidst.xyz` for this
staging host) with **Zone → DNS → Edit** permission. Certbot creates and removes
the `_acme-challenge` TXT records. Use a zone-scoped API token, not the Global
API Key. The [Certbot Cloudflare plugin documentation](https://certbot-dns-cloudflare.readthedocs.io/en/stable/)
describes this permission and credentials format.

## 2. Install Certbot and protect the token

On Ubuntu, with the Vorp `vorp` service user and group already created:

```sh
sudo apt-get update
sudo apt-get install certbot python3-certbot-dns-cloudflare
sudo install -d -o root -g vorp -m 0750 /etc/vorp
sudo touch /etc/letsencrypt/cloudflare.ini
sudo chown root:root /etc/letsencrypt/cloudflare.ini
sudo chmod 0600 /etc/letsencrypt/cloudflare.ini
sudoedit /etc/letsencrypt/cloudflare.ini
```

Put this one line in the editor, replacing the placeholder with the token:

```ini
dns_cloudflare_api_token = YOUR_ZONE_SCOPED_TOKEN
```

The credentials file must remain `root:root` and mode `0600`. Never commit the
token, paste it into a shell command, or pass it as a command-line argument.
Certbot records the *path* to this file in its renewal configuration.

## 3. Install the renewal hook and issue the certificate

From a Vorp checkout on the host:

```sh
sudo install -d -o root -g root -m 0755 /usr/local/libexec
sudo install -o root -g root -m 0755 deploy/cert-deploy.sh /usr/local/libexec/vorp-cert-deploy
sudo certbot certonly \
  --non-interactive --agree-tos --email admin@example.com \
  --dns-cloudflare \
  --dns-cloudflare-credentials /etc/letsencrypt/cloudflare.ini \
  --dns-cloudflare-propagation-seconds 30 \
  --key-type ecdsa \
  --cert-name vorp-staging.themidst.xyz \
  --deploy-hook /usr/local/libexec/vorp-cert-deploy \
  -d vorp-staging.themidst.xyz \
  -d '*.vorp-staging.themidst.xyz'
```

Replace the example email with your own address before running the command.
For another installation, replace every occurrence of the staging domain.
The explicit `--cert-name` gives the renewal lineage a predictable name.
The hook copies Certbot's renewed files into `/etc/vorp` as `root:vorp 0640`;
the Vorp service user can read them while Certbot's private archive remains
root-only. On the first issuance it also places the files needed to start Vorp.

Point Vorp at the copied files:

```sh
vorp serve \
  --base-domain vorp-staging.themidst.xyz \
  --dashboard-host vorp-staging.themidst.xyz \
  --tls-cert /etc/vorp/fullchain.pem \
  --tls-key /etc/vorp/privkey.pem \
  --database-path /var/lib/vorp/vorp.sqlite3
```

This is the staging service's effective configuration; use your own domain
and database path elsewhere. Vorp checks the certificate files every 30
seconds and uses a valid replacement for new TLS handshakes without a service
restart. Existing connections keep running.

## 4. Check issuance and renewal

```sh
sudo certbot certificates
sudo systemctl enable --now certbot.timer
sudo systemctl status certbot.timer
sudo certbot renew --dry-run
sudo stat -c '%U %G %a %n' /etc/vorp/fullchain.pem /etc/vorp/privkey.pem
curl -fsS -o /dev/null -w '%{http_code} TLS=%{ssl_verify_result}\n' \
  https://vorp-staging.themidst.xyz/healthz
```

Expect both the base and wildcard names in `certbot certificates`, an active
timer, a successful dry run, `root vorp 640` on both copied files, and
`200 TLS=0` from curl. A dry run tests ACME renewal but does not prove a
newly issued cert was loaded by the running relay. After a real renewal,
compare the certificate serial numbers in `/etc/letsencrypt/live/<cert-name>/`
and `/etc/vorp/fullchain.pem`, then make a new HTTPS connection. The
[Certbot renewal guide](https://eff-certbot.readthedocs.io/en/stable/using.html#renewing-certificates)
explains how renewal reuses the saved plugin settings and deploy hook.
Certbot may add a random delay before a noninteractive dry run; wait for its
final success or failure message.

Staging currently uses Certbot 4.0.0 and an ECDSA certificate for
`vorp-staging.themidst.xyz` and `*.vorp-staging.themidst.xyz`. The Cloudflare
token stays only on the host at `/etc/letsencrypt/cloudflare.ini`. GitHub
Actions deploys only the Vorp binary; it does not manage the token or TLS files.
