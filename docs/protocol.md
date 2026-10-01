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

There is no nginx and no SNI preread. The agent connects to the same `:443` as a
browser does; ALPN is what separates them.

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

The receiver re-establishes framing toward its own peer: set `Content-Length`
when `content_length` is `Some`, otherwise use chunked encoding. Both hyper and
the relay do this automatically from the body's known/unknown length.

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

### Error cases

| Situation | Behaviour |
|---|---|
| Agent cannot reach upstream | `ResponseHead` with status `502`, then `BodyEnd` |
| Upstream dies mid-body | `BodyAbort` — relay terminates the client response abruptly (a truncated chunked body, which is the honest signal) rather than sending a misleading complete one |
| Client disconnects mid-upload | Relay sends `BodyAbort`, agent aborts the upstream request |
| Malformed/unexpected frame | `Error` with `STREAM_ERROR`, then close the stream |
| No `ResponseHead` within the head timeout | Relay answers `504` and closes the stream |

A `BodyAbort` received before any `ResponseHead` becomes a `502` to the client.

### WebSocket upgrade

When `ResponseHead` carries status `101`, **framing stops**. Every byte after
that frame on that stream is raw tunnelled traffic, copied bidirectionally until
either side closes. The relay reconstructs the `101` toward the client on the
upgraded connection, clears the stream's idle timeout, and pipes.

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
  │                                                │  token → user (SHA-256 hash lookup,
  │                                                │    constant-time compare)
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

`machine_id` is `SHA-256(hostname + ":" + first_non_loopback_mac)`. It is
**client-supplied and not a credential** — two users on one host derive the same
value, so the session slot key is `(user_id, machine_id)`, never `machine_id`
alone. A reconnect displaces only that user's own previous session.

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
answered `431`. Body size limits are policy, enforced as a running byte counter
across chunks (`413`), default unlimited — streaming means they are no longer a
memory guard.

---

## 9. Timeouts

A single whole-round-trip deadline is wrong once bodies stream: it would sever
large uploads and SSE. Three narrower bounds replace it.

| Bound | Default | Applies to |
|---|---|---|
| head timeout | 30s | relay waiting for the first `ResponseHead` |
| stream idle timeout | 60s | any stream making no frame progress; reset per frame; **cleared** after a `101` |
| handshake timeout | 30s | reading `RegisterAgent` on a new control stream |

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
| `STREAM_ERROR` | unexpected frame or stream state | skip tunnel, continue |

Every code in this table is emitted by an implementation. **Do not define codes
nothing sends** — the Go version carried three (`SESSION_NOT_FOUND`,
`LOCAL_UNREACHABLE`, `TIMEOUT`) that no code path ever produced.

`AUTH_FAILED` and `UNSUPPORTED_VERSION` are permanent: the agent reports and
exits. Everything else is transient. Backoff applies only when establishing a
*new* session fails (`1s → 2s → 4s → 8s → 16s → 30s → 60s`); a live session that
drops reconnects immediately with no delay, because the link was just working.

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
