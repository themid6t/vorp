# Connecting services

How to put a service behind a vorp relay: tokens, names, running the agent,
keeping it running, upgrading and revoking. It assumes a relay is already set
up ([setup.md](setup.md)) and that you have a dashboard account on it.

Commands use `example.com` as the relay's base domain, `myapp` as a reserved
name, and a local service on `http://127.0.0.1:3000`. Substitute your own.

v0.0.1 has no `vorp login`, `vorp http` or `vorp service install`. They are
planned. Everything below uses the agent's flags.

> **A tunnel URL is not access control.** Anyone who has the URL can reach the
> service. Random names stop guessing, not sharing. Put authentication in the
> service for anything sensitive.

## How it fits together

- One agent process connects outbound to the relay on TCP 443 and serves one
  upstream URL. It can hold several tunnel names for that upstream (repeat
  `--subdomain`).
- The agent authenticates with an **agent token**. A token belongs to one
  user and has a **bind policy** that limits which names it may open.
- Each non-admin user has a quota: 3 live tunnels by default, across all their
  agents. Admins can change it.

## 1. Choose a bind policy

Create one token per machine or service, with the narrowest policy that works.

| Policy | No `--subdomain` (random name) | `--subdomain NAME` |
| --- | --- | --- |
| `temporary` | allowed | always rejected: `SUBDOMAIN_NOT_ALLOWED` |
| `any` | allowed | allowed if you own the reservation for `NAME`, or `NAME` is your assigned name |
| `reserved` | rejected: `SUBDOMAIN_NOT_ALLOWED` | allowed only if you own the reservation **and** `NAME` is in the token's allowlist |

The relay applies these rules in this order (the `decide_bind` function in
`crates/relay/src/authz.rs`):

1. No name requested: allowed, unless the policy is `reserved`
   (`SUBDOMAIN_NOT_ALLOWED`).
2. Policy `temporary` with a name: `SUBDOMAIN_NOT_ALLOWED`.
3. The name fails validation (see below): `SUBDOMAIN_INVALID`.
4. Another user holds the reservation: `SUBDOMAIN_TAKEN`.
5. You hold the reservation: allowed for `any`; for `reserved`, allowed only
   if the name is in the allowlist, else `SUBDOMAIN_NOT_ALLOWED`.
6. Nobody holds a reservation: `reserved` gets `SUBDOMAIN_NOT_ALLOWED`; `any`
   is allowed only for your assigned name, else `SUBDOMAIN_NOT_ALLOWED`.

After the policy allows a name, the registration can still fail with
`SUBDOMAIN_TAKEN` if another live tunnel already uses the name (for example,
a second agent for the same name), or with `TUNNEL_LIMIT` if your account
already has its quota of live tunnels.

Notes:

- A **pending** reservation request counts as "nobody holds a reservation".
  The name works only after an admin approves it.
- The **assigned name** is a 20-character hex name the relay gives a user the
  first time an `any` token asks for an unreserved name. `GET /api/me` shows
  it as `assigned_subdomain` (`null` until then). Most people use reserved
  names instead.
- A random name is new on every registration. The URL changes whenever the
  agent restarts or reconnects, including after a relay restart. Use a
  reserved name for anything that needs a stable URL.

## 2. Reserve a name

Name rules: 3 to 63 characters; lowercase letters, digits and hyphens; starts
and ends with a letter or digit. These are always refused: `www`, `api`,
`mail`, `smtp`, `ftp`, `admin`, `dash`, `dashboard`, `vorpd`.

**Dashboard:** open **Subdomains**, enter the name, and submit. Admins, and
users an admin allowed to reserve directly, get it at once. Everyone else
files a request; an admin approves it under **Admin**.

**API** (same-origin JSON API, see [api.md](api.md)). Log in once and keep the
session cookie in a private file:

```sh
J=$(mktemp)
printf '{"email":"%s","password":"%s"}' "you@example.com" "$PASSWORD" \
  | curl -fsS -c "$J" -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' \
      --data-binary @- https://example.com/api/login; echo
curl -sS -b "$J" -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' \
  -d '{"name":"myapp"}' https://example.com/api/reservations; echo
```

Use the dashboard host in the URLs (the base domain unless the relay was
started with `--dashboard-host`).

Expected: `{"ok":true}`, then `{"name":"myapp","status":"reserved"}` or
`{"name":"myapp","status":"pending"}`. Other answers:

| Response | Meaning |
| --- | --- |
| `400 {"error":"subdomain name"}` | The name breaks the rules above. |
| `409 {"error":"this name is already reserved by another user"}` | Pick another name. |
| `409 {"error":"another user has already requested this name"}` | Pick another name. |
| `409 {"error":"you have already reserved this name"}` | Nothing to do. |
| `403 {"error":"password change required"}` | Change the password first (dashboard **Account**, or `POST /api/password`). |

An admin approves a pending request with
`POST /api/admin/reservation-requests/myapp/approve`, which returns
`{"name":"myapp","user_id":2}`.

Check: `curl -fsS -b "$J" https://example.com/api/reservations` lists
`{"name":"myapp","status":"reserved"}`.

## 3. Create a token

**Dashboard:** open **Tokens**, choose the policy, tick the allowed names for a
`reserved` token, and create it. Copy the token immediately; it is shown once.

**API:**

```sh
curl -fsS -b "$J" -H 'X-Vorp-Csrf: 1' -H 'Content-Type: application/json' \
  -d '{"bind_policy":"reserved","allowlist":["myapp"]}' \
  https://example.com/api/tokens > token.json
jq '.token' token.json
```

Expected: `{"id": 3, "bind_policy": "reserved", "allowlist": ["myapp"], "revoked": false}`.
The secret is `raw_token` in the same response; it looks like
`vorp_<id>_<64 hex characters>`. Save it in step 4, then delete `token.json`.
Errors:

| Response | Meaning |
| --- | --- |
| `400 {"error":"allowlist requires reserved policy"}` | Only `reserved` tokens take an allowlist. |
| `400 {"error":"allowlist must contain owned reservations"}` | Every allowlist name must be a reservation you own (pending does not count). |
| `400 {"error":"bind policy"}` | `bind_policy` must be `any`, `temporary` or `reserved`. |

The relay stores only a SHA-256 hash of the token. A lost token cannot be
recovered; create a new one and revoke the old one.

## 4. Save the token

On the machine that runs the agent:

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
jq -r .raw_token token.json | vorp authtoken && rm token.json
```

Or paste it: run `vorp authtoken`, paste, press Enter, then Ctrl-D.

Expected log line: `agent token stored path=/home/you/.config/vorp/authtoken`.
The file is mode `0600`. Check with `stat -c '%a' ~/.config/vorp/authtoken`
(expected `600`).

Where the agent looks for the token, first match wins:

1. `--token VALUE`, or the `VORP_TOKEN` environment variable. Avoid both
   outside throwaway tests: `ps`, shell history and process managers can
   expose them. If `VORP_TOKEN` is set, `--token-file` is ignored.
2. `--token-file PATH`.
3. `$XDG_CONFIG_HOME/vorp/authtoken`, else `~/.config/vorp/authtoken`.

`vorp authtoken --token-file PATH` writes to `PATH` instead.

## 5. Run a tunnel

```sh
# Random name
vorp --relay-host example.com --upstream http://127.0.0.1:3000

# Reserved name
vorp --relay-host example.com --upstream http://127.0.0.1:3000 --subdomain myapp
```

Expected log lines:

```
INFO vorp_agent::session: agent connected machine_id=...
INFO vorp_agent::session: tunnel registered subdomain=myapp url=https://myapp.example.com
```

Check from anywhere:

```sh
curl -sS -o /dev/null -w '%{http_code}\n' https://myapp.example.com/
```

Expected: the status your service returns (for example `200`). `502` means
the relay reached the agent but the agent could not reach the upstream.

Agent flags:

| Flag | Meaning |
| --- | --- |
| `--relay-host` | The relay's name. The agent checks the relay certificate against it. Default `localhost`. |
| `--upstream` | Required. `http://host:port` only: no `https`, path, query, fragment or user info. |
| `--subdomain` | A name to claim. Repeat it for several names. Omit it for one random name. |
| `--allow-remote-targets` | Allow an upstream that is not loopback (`localhost`, `127.0.0.0/8`, `::1`). |
| `--token-file`, `--token` | See step 4. |
| `--relay-addr IP:PORT` | Dial this address instead of resolving `--relay-host` on port 443. |
| `--ca-cert PATH` | Extra CA certificate to trust, for test relays. |

Behaviour to rely on:

- The agent reconnects by itself after network failures and relay restarts.
- It exits with status 1 on permanent errors: a bad or revoked token, a
  protocol version mismatch, or when every requested tunnel was rejected or
  closed for good. A rejected `--subdomain` is logged and skipped; the agent
  keeps the other names.
- It exits with status 0 on SIGTERM or Ctrl-C.
- Closing a tunnel from the dashboard ends it for good: the agent logs
  `relay closed tunnel permanently` and, if that was its only tunnel, exits
  with status 1. A supervisor that restarts on failure brings it back. To stop
  a tunnel, stop the agent's service.

Log lines contain colour codes. Set `NO_COLOR=1` for plain text.

## 6. Keep it running

Pick one. Each uses a token **file**.

### systemd user service (Linux)

```sh
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/vorp-myapp.service <<'EOF'
[Unit]
Description=vorp tunnel for myapp

[Service]
ExecStart=/usr/local/bin/vorp --relay-host example.com --upstream http://127.0.0.1:3000 --subdomain myapp
Environment=NO_COLOR=1
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now vorp-myapp
sudo loginctl enable-linger "$USER"
```

Linger keeps user services running after logout and starts them at boot. The
token comes from `~/.config/vorp/authtoken` (step 4).

Check:

```sh
systemctl --user is-active vorp-myapp
loginctl show-user "$USER" -p Linger
journalctl --user -u vorp-myapp -n 20 --no-pager | grep 'tunnel registered'
```

Expected: `active`, `Linger=yes`, and a `tunnel registered ... url=https://myapp.example.com` line.

`Restart=always` also restarts after permanent errors, so a revoked token
shows up as `agent authentication failed` in the journal every few seconds.
Fix the cause, then `systemctl --user restart vorp-myapp`.

### pm2

Pass the token as a file only. pm2 shows a process's arguments and
environment, and `pm2 save` writes them to `~/.pm2/dump.pm2`, so `--token` or
`VORP_TOKEN` would leak the secret.

```sh
pm2 start /usr/local/bin/vorp --name vorp-myapp --interpreter none -- \
  --relay-host example.com --upstream http://127.0.0.1:3000 --subdomain myapp \
  --token-file "$HOME/.config/vorp/authtoken"
pm2 save
pm2 startup    # prints a command to run once with sudo, so pm2 starts at boot
```

`--interpreter none` runs the binary directly instead of through Node.

Check:

```sh
pm2 jlist | jq -r '.[] | select(.name=="vorp-myapp") | .pm2_env.status, (.pm2_env.args | join(" "))'
pm2 logs vorp-myapp --lines 20 --nostream | grep 'tunnel registered'
```

Expected: `online`, the arguments with `--token-file` and no token value, and
a `tunnel registered` line.

### Kubernetes sidecar

The agent runs as a second container in the application's Pod. Containers in
one Pod share the network namespace, so the upstream is loopback and
`--allow-remote-targets` is not needed.

There is no official image in v0.0.1. Build a small one from the verified
release binary. On a machine where the installer has put the binary at
`/usr/local/bin/vorp`, with the same CPU architecture as the cluster nodes:

```sh
mkdir vorp-image && cd vorp-image
cp /usr/local/bin/vorp .
cat > Dockerfile <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY vorp /usr/local/bin/vorp
USER 65534:65534
ENTRYPOINT ["/usr/local/bin/vorp"]
EOF
docker build -t registry.example.com/vorp-agent:0.0.1 .
docker run --rm registry.example.com/vorp-agent:0.0.1 --version
docker push registry.example.com/vorp-agent:0.0.1
```

Expected: `vorp 0.0.1`. The binary is static; `ca-certificates` lets it verify
the relay's Let's Encrypt certificate.

Store the token as a Secret, created from the file (not `--from-literal`,
which puts it in shell history):

```sh
kubectl create secret generic vorp-agent-token \
  --from-file=token="$HOME/.config/vorp/authtoken"
kubectl get secret vorp-agent-token -o jsonpath='{.data}' | jq 'keys'
```

Expected: `["token"]`.

Add the sidecar to the Deployment:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp
spec:
  replicas: 1            # one agent per reserved name; see below
  strategy:
    type: Recreate       # the old Pod must release the name first
  selector:
    matchLabels: {app: myapp}
  template:
    metadata:
      labels: {app: myapp}
    spec:
      containers:
        - name: app
          image: registry.example.com/myapp:latest
          ports:
            - containerPort: 3000
        - name: vorp
          image: registry.example.com/vorp-agent:0.0.1
          args:
            - --relay-host=example.com
            - --upstream=http://127.0.0.1:3000
            - --subdomain=myapp
            - --token-file=/etc/vorp/token
          env:
            - {name: NO_COLOR, value: "1"}
          volumeMounts:
            - {name: vorp-token, mountPath: /etc/vorp, readOnly: true}
          securityContext:
            runAsNonRoot: true
            readOnlyRootFilesystem: true
            allowPrivilegeEscalation: false
          resources:
            requests: {cpu: 10m, memory: 16Mi}
      volumes:
        - name: vorp-token
          secret:
            secretName: vorp-agent-token
```

Check:

```sh
kubectl rollout status deploy/myapp
kubectl logs deploy/myapp -c vorp | grep 'tunnel registered'
```

Expected: `deployment "myapp" successfully rolled out`, and a
`tunnel registered ... url=https://myapp.example.com` line.

Notes:

- **No probes.** The agent has no health endpoint. It exits on permanent
  errors, and Kubernetes restarts it with back-off, which shows up as
  `CrashLoopBackOff`. Read `kubectl logs ... -c vorp --previous` for the
  reason.
- **One agent per reserved name.** A second Pod claiming the same name gets
  `SUBDOMAIN_TAKEN` and exits. Keep `replicas: 1` with a reserved name, and use
  `Recreate` so a rollout does not start the new Pod while the old one still
  holds the name. With random names, each replica gets its own URL.
- **Agent in its own Deployment.** To run the agent apart from the app, point
  it at the Service and allow a non-loopback upstream:
  `--upstream=http://myapp:3000 --allow-remote-targets`.
- The agent needs a network interface with a MAC address (any normal Pod has
  one). Without one it exits with `no machine MAC address found`.

## 7. Upgrade

Agent and relay must speak the same protocol version. All v0.0.x releases use
protocol version 1. A mismatch makes the agent exit with
`relay protocol version is unsupported`.

- Host install: rerun the installer, then restart the agent
  (`systemctl --user restart vorp-myapp` or `pm2 restart vorp-myapp`). The
  installer keeps the previous binary as `/usr/local/bin/vorp.rollback`.
- Container: rebuild the image from the new binary with a new tag, and update
  the Deployment.

Check: `vorp --version` prints the new version, and the agent logs
`tunnel registered` again.

## 8. Revoke or rotate a token

Revoke in the dashboard (**Tokens**, then revoke), or with the API:

```sh
curl -fsS -b "$J" -X POST -H 'X-Vorp-Csrf: 1' https://example.com/api/tokens/3/revoke; echo
```

Expected: `{"ok":true}`. The relay closes every live session that
authenticated with that token. Each agent using it exits with:

```
Error: agent stopped

Caused by:
    agent authentication failed
```

To rotate: create a new token (step 3), save it (step 4, or replace the
Secret), restart the agent, check it logs `tunnel registered`, then revoke the
old token. Replace a Kubernetes Secret with:

```sh
kubectl create secret generic vorp-agent-token \
  --from-file=token="$HOME/.config/vorp/authtoken" --dry-run=client -o yaml \
  | kubectl apply -f -
kubectl rollout restart deploy/myapp
```

When finished with the API, end the session and delete the cookie file:

```sh
curl -fsS -b "$J" -X POST -H 'X-Vorp-Csrf: 1' https://example.com/api/logout; echo
rm -f "$J"
```

If something fails, see [troubleshooting.md](troubleshooting.md).
