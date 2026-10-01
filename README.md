# vorp

`vorp` is a self-hosted reverse tunnel relay. The same Rust binary runs the
relay (`vorp serve`) and an outbound-only agent (`vorp`). A single TLS listener
uses ALPN to separate agent sessions from the dashboard and public tunnel
traffic. Tunnel bodies stream through yamux in bounded frames.

The current implementation covers the [M1 and M2 milestones](docs/roadmap.md):
local TLS with certificate files, tunnels, the SQLite-backed dashboard, users,
sessions, agent tokens, bind policies, and live revocation. Certificate files
are checked every 30 seconds and reloaded for new TLS handshakes without
interrupting existing connections; an invalid replacement leaves the last
valid certificate in use. ACME automation, install helpers, and distribution
artifacts are still planned. See [the wire contract](docs/protocol.md) for
protocol details.

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

The relay listens on `0.0.0.0:443` by default. Open
`https://example.com/` and use **First-run admin** once to create the initial
account. Signup defaults to `closed`; `--signup open` and `--signup invite` are
available. Mint an agent token in the dashboard and copy it when shown; the raw
value cannot be retrieved later.

Store the token locally by passing it on standard input, then start the agent:

```sh
vorp authtoken < /path/to/token.txt
vorp --relay-host example.com --relay-addr 203.0.113.10:443 \
  --upstream http://127.0.0.1:3000
```

The token file defaults to `$XDG_CONFIG_HOME/vorp/authtoken`, or
`$HOME/.config/vorp/authtoken` where XDG is unset. `--token-file` chooses another
path; `VORP_TOKEN` can supply the token without a file. The agent rejects a
non-loopback upstream unless `--allow-remote-targets` is explicit.

For local development, `--listen 127.0.0.1:8443` with `VORP_DEV_TOKEN` lets the
relay run without the dashboard. Use `--dev-self-signed --dev-cert-out
./vorp-dev.crt` to create a temporary certificate, and point the agent's
`--ca-cert` at that file. Give the agent the same development token. The token
is accepted only with self-signed TLS or a loopback listener. A loopback relay
can also use `--tls-cert` and `--tls-key` with a local CA-signed certificate.

A high-entropy tunnel URL prevents casual guessing; it is not access control.
Anyone holding the URL can reach the tunneled application. Protect sensitive
upstreams with their own authentication.
