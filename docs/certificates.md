# TLS certificates with Cloudflare DNS and Certbot

In `serve` mode, vorp needs a certificate and private key supplied as
files (`--tls-cert`, `--tls-key`). Built-in ACME is planned
but not implemented. This page is the reference for
the procedure in [README step 4](../README.md#4-get-a-wildcard-certificate):
the same commands and paths, with the reasons and the checks.

It uses Let's Encrypt, Certbot and the Certbot Cloudflare DNS plugin. vorp
still terminates TLS itself on port 443. Nothing (no nginx, no Cloudflare
proxy) sits between agents or visitors and the relay.

## Why a wildcard and DNS-01

Tunnel names are random or user-chosen, so the relay needs one certificate
for `example.com` and `*.example.com`. Let's Encrypt issues wildcards only
through the DNS-01 challenge: Certbot proves control of the domain by
creating a `_acme-challenge` TXT record through the Cloudflare API. No inbound
HTTP is involved, so port 80 can stay closed.

A relay under a subdomain works the same way: a relay at `tunnels.example.com`
needs a certificate for `tunnels.example.com` and `*.tunnels.example.com`, and
A records for `tunnels` and `*.tunnels`.

## 1. DNS and the Cloudflare token

Create A records for `example.com` and `*.example.com` pointing at the relay's
public IP, both set to **DNS only** (grey cloud). The relay must receive the
TLS connection itself: agents negotiate the `vorp-agent/1` ALPN protocol,
which the Cloudflare proxy cannot pass through. Open TCP 443 on the server.

Create a Cloudflare API token scoped to the zone (`example.com`) with
**Zone → DNS → Edit**. Use a zone-scoped API token, not the Global API Key.
The [Certbot Cloudflare plugin documentation](https://certbot-dns-cloudflare.readthedocs.io/en/stable/)
describes the permission and the credentials format.

## 2. Service user, Certbot and the credentials file

```sh
sudo useradd --system --home-dir /var/lib/vorp --shell /usr/sbin/nologin vorp
sudo install -d -o vorp -g vorp -m 0750 /var/lib/vorp
sudo install -d -o root -g vorp -m 0750 /etc/vorp

sudo apt-get install -y certbot python3-certbot-dns-cloudflare sqlite3
sudo install -d -m 0755 /etc/letsencrypt
sudo install -m 0600 /dev/null /etc/letsencrypt/cloudflare.ini
sudoedit /etc/letsencrypt/cloudflare.ini
```

`/etc/vorp` is `root:vorp 0750`: the relay can read the copied certificate
but cannot change it. `install -m 0600 /dev/null` creates the credentials file
empty and `root`-only before the secret goes in. Put one line in it:

```ini
dns_cloudflare_api_token = YOUR_ZONE_SCOPED_TOKEN
```

Keep the file `root:root 0600`. Never commit the token, paste it into a shell
command, or pass it as a command-line argument. Certbot stores only the
*path* to this file in its renewal configuration.

Check: `sudo stat -c '%U %G %a' /etc/letsencrypt/cloudflare.ini` prints
`root root 600`. [setup.md](setup.md#step-3-install-certbot-and-store-the-cloudflare-token)
shows how to test the token against the Cloudflare API without exposing it.

## 3. The deploy hook

Certbot keeps certificates under `/etc/letsencrypt/live/<cert-name>/`, which
only root can read. The deploy hook copies the two files vorp needs into
`/etc/vorp`:

```sh
sudo install -d /usr/local/libexec
sudo curl -fsSL -o /usr/local/libexec/vorp-cert-deploy \
  https://raw.githubusercontent.com/themid6t/vorp/main/deploy/cert-deploy.sh
sudo chmod 0755 /usr/local/libexec/vorp-cert-deploy
```

From a checkout of the repository, `sudo install -m 0755 deploy/cert-deploy.sh
/usr/local/libexec/vorp-cert-deploy` does the same.

The hook ([`deploy/cert-deploy.sh`](../deploy/cert-deploy.sh)) is four lines.
Certbot sets `RENEWED_LINEAGE` to the certificate's `live` directory, and the
hook installs `fullchain.pem` and `privkey.pem` into `/etc/vorp` as
`root:vorp 0640`. It needs the `vorp` group and `/etc/vorp` to exist.

## 4. Issue the certificate

```sh
sudo certbot certonly --non-interactive --agree-tos --email you@example.com \
  --dns-cloudflare --dns-cloudflare-credentials /etc/letsencrypt/cloudflare.ini \
  --dns-cloudflare-propagation-seconds 30 --key-type ecdsa \
  --cert-name example.com --deploy-hook /usr/local/libexec/vorp-cert-deploy \
  -d example.com -d '*.example.com'
```

- `--email`: your address, for Let's Encrypt expiry notices.
- `--dns-cloudflare-propagation-seconds 30`: how long Certbot waits after
  creating the TXT record before asking Let's Encrypt to check it.
- `--key-type ecdsa`: a smaller, faster key; rustls supports it.
- `--cert-name example.com`: a predictable lineage name, so the files live in
  `/etc/letsencrypt/live/example.com/`.
- `--deploy-hook`: runs after this issuance and after every later renewal.
  Certbot saves it in the renewal configuration.

Check:

```sh
sudo certbot certificates
sudo stat -c '%U %G %a %n' /etc/vorp/fullchain.pem /etc/vorp/privkey.pem
sudo openssl x509 -in /etc/vorp/fullchain.pem -noout -ext subjectAltName
```

Expected: `certbot certificates` lists the `example.com` lineage with
`Domains: example.com *.example.com`; both files are `root vorp 640`; the
subject alternative names are `DNS:example.com` and `DNS:*.example.com` (in
either order).

### Test the hook by hand

If Certbot succeeded but `/etc/vorp/*.pem` is missing or stale, run the hook
the way Certbot does:

```sh
sudo env RENEWED_LINEAGE=/etc/letsencrypt/live/example.com /usr/local/libexec/vorp-cert-deploy
sudo stat -c '%U %G %a %n' /etc/vorp/fullchain.pem /etc/vorp/privkey.pem
```

Expected: no output from the hook, then `root vorp 640` for both files. An
`install: invalid group 'vorp'` error means the `vorp` user and group do not
exist yet.

## 5. Point the relay at the copies

[`deploy/vorp.service`](../deploy/vorp.service) runs:

```sh
/usr/local/bin/vorp serve --base-domain example.com \
  --tls-cert /etc/vorp/fullchain.pem --tls-key /etc/vorp/privkey.pem \
  --database-path /var/lib/vorp/vorp.sqlite3
```

The relay re-reads both files every 30 seconds. When their content changes
and the new pair is valid, it logs `TLS certificate reloaded` and uses the new
certificate for new TLS handshakes. Existing connections keep running; no
restart is needed. If the new files are unreadable or invalid, it logs
`TLS certificate reload failed; retaining the last valid certificate` and keeps
serving the old one.

## 6. Renewal

The Debian and Ubuntu `certbot` package installs `certbot.timer`, which runs
`certbot renew` twice a day. Renewal reuses the saved plugin settings and the
deploy hook; see the
[Certbot renewal guide](https://eff-certbot.readthedocs.io/en/stable/using.html#renewing-certificates).

```sh
systemctl is-enabled certbot.timer
sudo certbot renew --dry-run
```

Expected: `enabled`, and a dry run that ends by reporting that the simulated
renewal succeeded. Certbot may add a random delay before a non-interactive
dry run; wait for its final message.

A dry run tests the ACME side only. It does not run the deploy hook and does
not prove the relay loaded a new certificate. After a real renewal, compare
the serial numbers of the Certbot copy, the vorp copy, and what the relay
serves:

```sh
sudo openssl x509 -noout -serial -in /etc/letsencrypt/live/example.com/fullchain.pem
sudo openssl x509 -noout -serial -in /etc/vorp/fullchain.pem
openssl s_client -connect example.com:443 -servername example.com </dev/null 2>/dev/null \
  | openssl x509 -noout -serial
journalctl -u vorp --since '-1 day' --no-pager | grep 'TLS certificate'
```

Expected: the same `serial=` three times (allow 30 seconds after the hook
ran), and a `TLS certificate reloaded` line.

## Where the secrets live

The Cloudflare token stays on the relay host in
`/etc/letsencrypt/cloudflare.ini`. The private key exists in Certbot's
archive (root only) and as `/etc/vorp/privkey.pem` (readable by the `vorp`
group). Neither belongs in the repository, a CI system, or a backup that
leaves the host unencrypted.
