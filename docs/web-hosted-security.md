# Hosted web identity and admission security

The browser service uses anonymous guest identities, not device fingerprints
or itch.io cookies. `POST /v2/guests` validates a Unicode-normalized nickname,
creates a random guest identifier, and assigns a
separate random 64-bit authority identity. The returned bearer session is
HMAC-SHA-256 authenticated, expires after 24 hours by default, and contains no
name, address, or platform identifier.

Room membership alone cannot open a gameplay socket. Once a host seals a room,
each member requests a room-, match-, peer-, guest-, and connection-mode-scoped
join ticket. Join tickets expire after 30 seconds by default and carry a random
128-bit nonce. The server atomically consumes that nonce before attaching a
transport to `AuthorityPeerHub`; replay and replay-cache exhaustion fail
closed.

Both token kinds use fixed-size, versioned binary claims and URL-safe base64
without padding. Key identifiers support one current and one previous signing
key, allowing rotation without immediately invalidating existing guest
sessions. New tickets always use the current key. Production startup must load
the current 32-byte key from secret management; it must never be committed,
placed in a URL, or emitted to logs.

The gameplay transport begins with a bounded admission frame outside the AFC
wire protocol. WebSocket admission uses the first reliable binary message.
WebTransport admission uses a reliable bidirectional stream. Only an accepted
connection transitions to opaque AFC datagrams, so ticket bytes never enter
simulation, replay, canonical events, or protocol diagnostics.

Operational requirements:

- Terminate only HTTPS/WSS and HTTP/3 with publicly trusted certificates.
- Allow only explicitly configured itch.io/custom-domain origins; never combine
  wildcard origins and credentials.
- Keep request limits, room limits, admission timeouts, and transport queues
  bounded.
- Keep guest bearers in per-tab `sessionStorage` and WASM only; never expose
  them through DOM state, URLs, analytics, crash reports, or chat payloads.
- Render nicknames and chat with text nodes, enforce server-side grapheme/byte
  limits and rate limits, and retain bounded mute/report controls. Room kicks
  ban that guest identity for the room lifetime.
- Redact bearer sessions, join tickets, admission frames, room codes, and
  signing keys from access/error logs.
- Rotate signing keys by installing the old current key as previous, deploying
  the new current key, waiting at least the guest-session TTL, then removing the
  previous key.
