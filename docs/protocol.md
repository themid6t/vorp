# vorp wire protocol

The contract between the `vorp` agent and the `vorp serve` relay. Implemented in
`crates/protocol`, which owns every byte that crosses the wire and is the only
place frames are constructed or parsed.

Status: **v1 draft.** This document is the spec; the crate must match it, and a
change here is a change to both sides.

---

## 1. Transport

One outbound TLS connection per agent, multiplexed with yamux.

```
agent ──TLS 1.3 (ALPN "vorp-agent/1")──> relay :443
          └── yamux session
                ├── control stream   agent-opened, one per session
                ├── tunnel stream    agent-opened, one per tunnel
                ├── request stream   RELAY-opened, one per HTTP request
                └── request stream   ...
```

The relay runs a single `:443` rustls acceptor and dispatches on the negotiated
ALPN protocol:

| ALPN | Handler |
|---|---|
| `vorp-agent/1` | yamux session handler (this protocol) |
| `h2`, `http/1.1` | axum — dashboard, `/api`, and the public tunnel proxy, dispatched by `Host` |
| no ALPN extension | HTTP/1.1 dashboard or public tunnel proxy |

There is no nginx and no SNI preread. The agent connects to the same `:443` as a
browser does; ALPN is what separates them.
The relay accepts TLS 1.3 and 1.2 (rustls' AEAD-only 1.2 suites), so public
tunnel visitors on older clients can connect; the agent negotiates 1.3.

WebSocket upgrades use HTTP/1.1. The relay does not advertise HTTP/2 extended
`CONNECT` support; an HTTP/2 request cannot request a WebSocket tunnel through
this protocol.

**The agent only ever dials out.** Inbound work reaches it because the *relay*
opens a new yamux stream for each HTTP request. This is the whole reason the
agent works behind NAT, and no change may require the agent to listen.

---

## 2. Frame format

Every frame on every stream:

```
┌────────┬──────────────────┬───────────────────┐
│ 1 byte │ 4 bytes (BE u32) │ payload_len bytes │
│  type  │   payload_len    │     payload       │
└────────┴──────────────────┴───────────────────┘
```

`payload_len` **must not exceed 65536 (64 KiB)** for any frame type. A receiver
that reads a larger declared length aborts the stream immediately without
allocating — this is the memory-exhaustion guard, so it is checked before the
allocation, never after.

Payload encoding depends on the type:

- **Control frames** — JSON (`serde_json`). Low frequency; the readability in a
  packet capture is worth more than the bytes.
- **`BodyChunk`** — raw opaque bytes, no encoding, no framing of its own.

A frame's type determines which it is; there is no flag to inspect.

The frame reader consumes exactly the five-byte prefix and declared payload.
It must not read ahead into bytes after the frame: a `101` response switches the
same stream to raw WebSocket bytes, which must remain available to the raw copier.
If a buffered reader is introduced, its unread buffer must be handed to that
copier. A sender writes each complete frame from one contiguous buffer and
awaits the write. This does not make transport writes atomic: cancellation or an
I/O error can leave a partial frame. After either, close the stream; never send
another frame on it.

### Header values must be UTF-8

Header names and values travel as JSON strings. A header value containing
non-UTF-8 bytes is rejected at the proxy boundary (`400` toward the client,
`502` toward the agent) rather than transcoded or dropped.

> **Known ceiling:** HTTP permits opaque bytes in header values, so this refuses
> a small class of non-conforming traffic (almost exclusively a raw filename in
> an unencoded `Content-Disposition`). Upgrade path if it ever bites a real user:
> switch control-frame encoding to CBOR/msgpack behind the same codec API, which
> changes no call sites.

---

## 3. Message types

Grouped by function, with gaps left for growth.

### Control stream

| Type | Name | Direction | Payload |
|---|---|---|---|
| `0x01` | `RegisterAgent` | A→R | `{ protocol_version: u32, token: String, machine_id: String }` |
| `0x02` | `AgentAck` | R→A | `{ session_id: String, server_version: u32 }` |
| `0x03` | `AgentErr` | R→A | `{ code: String, message: String }` |
| `0x04` | `Ping` | A→R | `{ timestamp_ms: i64 }` |
| `0x05` | `Pong` | R→A | `{ timestamp_ms: i64 }` (echoed) |

### Tunnel stream

| Type | Name | Direction | Payload |
|---|---|---|---|
| `0x10` | `RegisterTunnel` | A→R | `{ subdomain: Option<String>, upstream_hint: Option<String> }` |
| `0x11` | `TunnelAck` | R→A | `{ subdomain: String, url: String }` |
| `0x12` | `TunnelErr` | R→A | `{ code: String, message: String }` |
| `0x13` | `TunnelClose` | R→A | `{ subdomain: String, reason: String }` |
| `0x14` | `TunnelCloseAck` | A→R | `{ subdomain: String }` |

`upstream_hint` is cosmetic — it exists only so the dashboard can show what the
agent is forwarding to. **The relay never uses it for routing.** The upstream is
fixed by agent configuration and is never influenced by a public caller.

### Request stream

| Type | Name | Direction | Payload |
|---|---|---|---|
| `0x20` | `RequestHead` | R→A | see §5 |
| `0x21` | `ResponseHead` | A→R | see §5 |
| `0x30` | `BodyChunk` | either | raw bytes, ≤ 64 KiB |
| `0x31` | `BodyEnd` | either | `{}` — body complete |
| `0x32` | `BodyAbort` | either | `{ message: String }` — body truncated, do not treat as complete |
| `0x3F` | `Error` | either | `{ code: String, message: String }` |

`TunnelClose` is only ever relay→agent. The agent never initiates one; it
receives one and replies `TunnelCloseAck`.

---

## 4. Streaming is mandatory

**A proxied body is never buffered whole, in either direction, on either side.**

This is the one rule that shapes the whole protocol. Bodies move as a sequence of
`BodyChunk` frames terminated by `BodyEnd` (or `BodyAbort`). No participant may
collect a body into a `Vec`/`Bytes` before forwarding it, and no participant may
read ahead into an unbounded queue.

Backpressure must propagate end to end through the yamux stream window:

```
browser ←TCP← relay ←yamux window← agent ←TCP← upstream
```

A sender writes one chunk and **awaits that write**. When the receiver stops
reading, the yamux window closes, the write stalls, the sender stops reading from
its own source, and TCP backpressure reaches the original peer. Spawning a
forwarding task with an unbounded channel between the read and the write breaks
this chain and is a bug, not an optimisation.

Consequences of getting this right, and the reasons it is a rule:

- An upload larger than RAM proxies fine.
- SSE and long-poll responses arrive incrementally instead of sitting in a buffer
  until a deadline fires.
- Body size limits become *policy* (reject with `413`) rather than the thing
  standing between the relay and an OOM.

The Go implementation this replaces serialized each whole HTTP message with
`httputil.DumpRequest`/`DumpResponse` and buffered it at 64 MiB on both sides.
That is the specific design being discarded.

---

## 5. Body framing is protocol metadata, not a header

`Content-Length` and `Transfer-Encoding` are **stripped** from the forwarded
header list and never appear in `headers`. The declared length travels as a
dedicated field, and the protocol's own chunk/end frames carry the framing:

```jsonc
// 0x20 RequestHead  (relay → agent)
{
  "subdomain": "k3f9x2",
  "method": "POST",
  "target": "/v1/upload?async=1",   // path + query, origin-form
  "headers": [                       // ordered, duplicates preserved
    ["host", "k3f9x2.example.com"],
    ["content-type", "application/json"],
    ["x-forwarded-for", "203.0.113.7"]
  ],
  "content_length": 4096             // null when unknown (chunked/streamed)
}

// 0x21 ResponseHead  (agent → relay)
{
  "status": 200,
  "headers": [["content-type", "text/event-stream"]],
  "content_length": null
}
```

The receiver re-establishes framing toward its own peer. `content_length` is
metadata, not proof that the promised bytes arrived. The agent may use a known
request length when forwarding to its upstream. The relay treats a response
length supplied by the agent as unverified and streams the client response with
unknown length; HTTP/1.1 uses chunked encoding, while HTTP/2 uses DATA frames.
It must not advertise an unverified `Content-Length` to the browser, because a
truncated upstream response would otherwise look complete to that browser.

This design removes request smuggling **by construction** rather than by
validation — there is no path by which a client-supplied framing header reaches
the far side, because framing headers are not forwarded at all. The validation in
§8 stays anyway, as defence in depth at the point of entry.

`headers` is an ordered list of `[name, value]` pairs, not a map: duplicates are
legal and significant (`Set-Cookie`), and order must survive.

---

## 6. Request stream lifecycle

One HTTP request/response per stream. The stream is the correlation key — there
are no request IDs, and none should be added.

```
relay                                            agent
  │                                                │
  ├─ yamux.open_stream() ─────────────────────────>│
  ├─ RequestHead ─────────────────────────────────>│  dial upstream, send head
  ├─ BodyChunk ───────────────────────────────────>│  forward chunk (awaited)
  ├─ BodyChunk ───────────────────────────────────>│
  ├─ BodyEnd ─────────────────────────────────────>│  finish upstream request
  │                                                │
  │<──────────────────────────────── ResponseHead ─┤  upstream responded
  │<────────────────────────────────── BodyChunk ──┤
  │<────────────────────────────────── BodyChunk ──┤
  │<──────────────────────────────────── BodyEnd ──┤
  └─ close stream                                  │
```

**The stream is full duplex.** The agent may send `ResponseHead` before it has
received `BodyEnd` — an upstream that rejects an upload early (`413`, `401`) must
be able to answer without the relay having to finish sending. Neither side may
block its reader on its writer completing.

### Body state and errors

Each direction has its own state: `RequestHead` or `ResponseHead`, then zero or
more `BodyChunk` frames, then exactly one `BodyEnd` or `BodyAbort`. The response
head may arrive while request chunks are still flowing. `BodyEnd` means complete;
`BodyAbort` means truncated and is terminal for the entire request stream. After
an abort, close the stream and send no further frames. Two aborts crossing in
flight are both treated as teardown, not as a second protocol error. A frame
after `BodyEnd` in the same direction, or any frame outside this sequence, is
`STREAM_ERROR`; close the stream.

If the agent cannot reach the upstream, it sends `ResponseHead` with status
`502`, then `BodyEnd`. If the upstream dies after `ResponseHead`, it sends
`BodyAbort`; the relay cuts off the client response, leaving an honestly
truncated body. If the client disconnects or its upload fails before `BodyEnd`,
the relay sends `BodyAbort` when the stream is still writable, then closes it;
the agent aborts its upstream request and owes no response. If sending the abort
also fails, closing the stream is the fallback. An agent `BodyAbort` before
`ResponseHead` is invalid; the relay returns `502` if it can still answer and
closes the stream. Abort messages are diagnostic only: never forward them to an
HTTP client or log untrusted text at a level that an agent can flood.

For any other malformed or unexpected frame, send `Error` with `STREAM_ERROR`
when possible, then close. If no `ResponseHead` arrives before the head timeout,
the relay answers `504` and closes the stream.

### WebSocket upgrade

For a validated WebSocket handshake, the relay writes `RequestHead`, then
`BodyEnd`, and no further request frames. The agent must receive that `BodyEnd`
before it may send a `101` response. This completes the framed request direction
before either peer can switch to raw bytes.

When `ResponseHead` carries status `101`, **framing stops**. The agent switches
after writing that frame; the relay switches after reading it. Every byte after
that frame on the same stream is raw tunnelled traffic, copied bidirectionally
until either side closes. The relay reconstructs the `101` toward the client on
the upgraded connection, clears the stream's idle timeout, and pipes. A non-101
response to an upgrade request is an ordinary framed response: its status,
headers, chunks and end/abort are forwarded normally.

This is an exception to the frame format, not to §4 — raw copying is streaming by
definition.

---

## 7. Handshake and session

```
agent                                             relay
  ├─ TLS connect, ALPN "vorp-agent/1" ───────────>│  reject if ALPN mismatched
  │<───────────────────────────── yamux server ────┤
  ├─ open control stream ─────────────────────────>│
  ├─ RegisterAgent ───────────────────────────────>│  version must equal 1
  │                                                │  public token-id prefix → row;
  │                                                │    constant-time compare of SHA-256 hashes
  │                                                │  displace any live session on the
  │                                                │    same (user_id, machine_id) slot,
  │                                                │    tearing it down with FORCED first
  │<──────────────────────────────── AgentAck ─────┤
  │                                                │
  ├─ open tunnel stream ──────────────────────────>│
  ├─ RegisterTunnel ──────────────────────────────>│  bind ACL decision (pure fn)
  │<────────────────────────────── TunnelAck ──────┤  { subdomain, url }
  │                                                │
  ├─ Ping (every 20s, control stream) ────────────>│
  │<──────────────────────────────────── Pong ─────┤
```

`machine_id` is `SHA-256(hostname + ":" + first_non_loopback_mac + ":" + pid +
":" + process_start_nanos)`, computed once per agent process. The process part
lets several agents on one host and account coexist; reconnects reuse the
value, so a process still displaces its own stale session. It is
**client-supplied and not a credential** — two users on one host derive the same
value, so the session slot key is `(user_id, machine_id)`, never `machine_id`
alone. A reconnect displaces only that user's own previous session.
Registration publication and live-token revocation are ordered under the same
registry lock: if revocation wins, publication is rejected; if publication
wins, revocation finds and tears down that session. Authentication before
publication alone is insufficient, because a revoke can commit between them.

### Heartbeat

Deliberately asymmetric. The agent probes the link it depends on and fails fast
because it alone decides when to reconnect; the relay waits longer because
tearing down a session is the more disruptive move.

| | Agent (sender) | Relay (reader) |
|---|---|---|
| interval | 20s | 20s |
| timeout | 5s (`< interval`) | 25s (`>= interval`) |
| max missed | 3 | 3 |

A ping whose `timestamp_ms` is older than `2 × interval` counts as missed.

### Tunnel close reasons

| Reason | Meaning | Agent action |
|---|---|---|
| `FORCED` | Dashboard/API force-close, or session displaced | Remove, do **not** re-register |
| `REVOKED` | Authenticating token revoked | Remove, do **not** re-register |
| `EXPIRED` | Absolute lifetime cap reached | Remove, do **not** re-register |
| `RECOVERABLE` | Transient relay-side condition | Re-register on a fresh stream, bounded backoff `500ms → 1s → 2s → 4s → 8s` |

On a recoverable close the agent replays its **originally requested** subdomain:
an auto-slug tunnel asks for a fresh slug, an explicit name re-claims that name.
A tunnel is never closed for inactivity — a healthy tunnel stays registered as
long as its session is healthy.

---

## 8. Proxy-boundary normalization

The relay normalizes at the edge, before anything is forwarded. Ported from the
Go `internal/server/httpnorm.go`; all of it is load-bearing.

**Reject ambiguous framing with `400`** — both `Content-Length` and
`Transfer-Encoding` present, multiple disagreeing `Content-Length` values, a
non-numeric length, or any transfer coding other than `chunked`. Redundant with
§5 by construction, kept as defence in depth at the entry point.

**Strip hop-by-hop headers**, in both directions: `Connection`,
`Proxy-Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`,
`TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`, plus every header named in the
sender's own `Connection` header (RFC 7230 §6.1). `Upgrade` survives **only** for
a validated WebSocket handshake (`Upgrade: websocket` with an `upgrade` token in
`Connection`); every other upgrade is dropped.

**Assert forwarding headers, never trust them.** The relay is the first hop
(there is no nginx in front), so it overwrites rather than appends:

- `X-Forwarded-For` ← the TLS peer address, single hop. The client-supplied chain
  is **discarded**, not extended.
- `X-Forwarded-Host` ← the requested host.
- `X-Forwarded-Proto` ← `https`.

A client cannot spoof the proxy chain or its own identity.

**Size bounds.** A serialized `RequestHead` exceeding the 64 KiB frame cap is
answered `431`. Body-size policy is not yet implemented; bodies currently have
no configured byte limit. Streaming avoids whole-body allocation, but does not
prevent bandwidth or upstream-disk exhaustion. A future limit must count bytes
as they pass and reject oversized declared lengths early with `413`.

**Edge connection bounds.** The relay accepts at most 1,024 concurrent TLS
connections, including handshakes, and 64 from one peer IP. It allows 256
concurrent HTTP tunnel requests, 128 upgraded WebSockets globally, and 128 of
each per tunnel. A per-peer token bucket allows 200 requests/second with a
400-request burst. Excess connections close before HTTP dispatch; exhausted
HTTP/WebSocket slots receive `503`, while rate-limited requests receive `429`.
Authentication attempts are separately rate-limited before argon2id work.
The connection and request-rate bounds are defaults for `--max-connections`,
`--max-connections-per-ip`, and `--rate-limit-rps`. The per-IP connection cap
counts agents and HTTP clients together, so a site behind one NAT shares it.
Run the relay with an open-file limit above `--max-connections` (systemd
defaults the soft limit to 1024): set `LimitNOFILE=65536` in its unit.

**Per-user quotas.** On top of those survival bounds, each non-admin user has
a quota shared across all of their agents and tunnels: live tunnels (default
3), concurrent HTTP requests (default 64), and bandwidth (default 10 MiB/s,
uploads and downloads combined, with one second of burst). Admins and
development-token sessions are exempt. An admin changes the defaults and
per-user overrides from the dashboard; changes apply to live sessions without
a reconnect, except that lowering the tunnel cap blocks new registrations and
leaves existing tunnels open.

The quotas slow traffic before they refuse it:

- **Bandwidth never drops.** Each body chunk (and each WebSocket read) is
  charged before it is forwarded; an over-budget transfer pauses, and the
  pause propagates through the yamux window and TCP to the sender.
- **Requests queue.** A request over the user's concurrency limit waits up to
  10s for a slot. At most four requests per slot may wait; beyond that, or
  after the wait, the request receives `503` with `Retry-After: 1`. The user
  slot is taken before the relay-wide slot, so a user queued behind their own
  quota does not hold global capacity. The head timeout (§9) starts at
  dispatch, so time spent queued does not count against it. An upgraded
  WebSocket releases its request slot.
- **Tunnels are refused.** A registration beyond the cap receives
  `TUNNEL_LIMIT`; it never displaces a live tunnel.

---

## 9. Timeouts

A single whole-round-trip deadline is wrong once bodies stream: it would sever
large uploads and SSE. Narrower bounds replace it.

| Bound | Default | Applies to |
|---|---|---|
| head timeout | 30s | relay waiting for the first `ResponseHead`; reset by progress on that request's upload stream, so an active upload is not cut off |
| stream idle timeout | 60s | any stream making no frame progress; reset per frame; **cleared** after a `101` |
| handshake timeout | 30s | reading `RegisterAgent` on a new control stream |
| TLS handshake timeout | 30s | new accepted TLS connections |
| HTTP/1 header read timeout | 10s | each request header block; backed by a Hyper timer |
| HTTP/1 connection buffer | 64 KiB | bounds the parser buffer, including headers |
| HTTP/2 stream limit | 128 | concurrent streams on one HTTP/2 connection |
| HTTP/2 header list | 64 KiB | decoded request headers on one HTTP/2 stream |

---

## 10. Subdomain assignment

With no `subdomain` in `RegisterTunnel`, the relay assigns one: **16 characters
of lowercase base32 (`[a-z2-7]`) from 80 bits of CSPRNG entropy**, unbiased
(every character is exactly 5 random bits), DNS-label safe.

A CSPRNG failure **fails the registration**. There is no non-cryptographic
fallback — the Go version's `math/rand` slug generator was a real hardening gap.

Insertion is an atomic check-and-set against the tunnel map; auto-slug generation
retries a bounded number of times on collision, which the entropy makes
astronomically unlikely and the bound only guards against a wedged map.

> High entropy keeps a URL from being **guessed**. It is not access control. A
> live tunnel is reachable by anyone holding the URL, so a public URL never
> substitutes for authentication in the tunnelled application.

---

## 11. Error codes

| Code | Meaning | Agent behaviour |
|---|---|---|
| `AUTH_FAILED` | token rejected | permanent — exit |
| `UNSUPPORTED_VERSION` | protocol version mismatch | permanent — exit |
| `SUBDOMAIN_TAKEN` | already registered, or reserved by another user | skip tunnel, continue |
| `SUBDOMAIN_INVALID` | fails label validation | skip tunnel, continue |
| `SUBDOMAIN_NOT_ALLOWED` | outside this token's bind ACL | skip tunnel, continue |
| `TUNNEL_LIMIT` | user already holds their quota of live tunnels | skip tunnel, continue |
| `STREAM_ERROR` | unexpected frame or stream state | skip tunnel, continue |

Every code in this table is emitted by an implementation. **Do not define codes
nothing sends** — the Go version carried three (`SESSION_NOT_FOUND`,
`LOCAL_UNREACHABLE`, `TIMEOUT`) that no code path ever produced.

`AUTH_FAILED` and `UNSUPPORTED_VERSION` are permanent: the agent reports and
exits. Everything else is transient. Backoff applies only when establishing a
*new* session fails (`1s → 2s → 4s → 8s → 16s → 30s → 60s`, each randomized to
between half and the full step); a live session that drops reconnects after a
random 0–5s wait. The wait is short because the link was just working, and
random so that every agent of a restarted relay does not redial at once — on a
small host that herd re-exhausted memory and crashed the relay in a loop.
Forced or expired tunnel closures remain suppressed across reconnects; if all
configured tunnels are suppressed, the agent exits instead of repeatedly
reclaiming a displaced machine slot with no usable tunnel.

---

## 12. Versioning

`PROTOCOL_VERSION: u32 = 1`, sent in `RegisterAgent` and echoed in `AgentAck`.
A mismatch is rejected with `UNSUPPORTED_VERSION`.

There is **no version-branching machinery**, and none should be written until a
v2 actually exists. Negotiation happens on the first frame, before any tunnel or
request work, so nothing downstream needs to know which version is live — the
branch can be added at that single point when it is needed.

Backwards-compatible without a bump: adding an optional field (unknown fields are
ignored — `serde` must be configured to permit them), adding a new message type
that is not required for correctness on both sides. Breaking, requiring a bump:
removing or renaming a field, changing a type's semantics, making an optional
field required.
