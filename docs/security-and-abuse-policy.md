# Multiplayer Security and Abuse Policy

- Status: authority enforcement and native Steam revocation/ban forwarding implemented; live validation pending
- Applies to: listen and dedicated online authorities
- Source: `src/multiplayer_security.rs`
- Decision date: 2026-07-23

The authority accepts input intent only. It never accepts a client-authored
position, contact, damage value, score, entity spawn, stock loss, or result. Steam
authentication and transport encryption establish identity and privacy; they do
not make a listen host suitable for trusted rewards.

## Enforcement layers

Traffic is rejected as early as possible and remains bounded at every layer:

1. The transport rejects datagrams over 1,200 bytes before copying them.
2. The runtime validates envelope version, channel, direction, message length,
   compatibility identity, ordering windows, and bounded queue capacity.
3. The session gate validates phase, match, peer, clock samples, deadlines, and
   manifest agreement.
4. The authority hub validates authenticated peer identity, seat ownership,
   monotonic/bounded input frames, reconnect claims, and resync identity.
5. Canonical simulation code calculates all gameplay outcomes.

Receive/send work is capped per network tick. Repeated budget exhaustion is an
abuse signal rather than permission to grow queues or monopolize the authority
worker. Malformed and abusive peers are isolated; one peer cannot stop another
peer's runtime from being pumped.

## Stable violation classes

The policy classifies violations without retaining raw packet strings or ticket
contents. Low-cost parse failures accumulate slowly. Direction, queue, and reliable
window abuse accumulate faster. Conflicting idempotent messages, invalid input, or
illegal session transitions are severe. Spoofed identity and seat ownership are
immediate kick-level events. Authentication revocation and platform/publisher bans
are immediate terminal events.

The default score thresholds are:

| Action | Score |
| --- | ---: |
| UI/telemetry warning | 8 |
| Disconnect/kick | 24 |
| Temporary local ban | 48 |

Scores decay by one point per second of clean 60 Hz authority time. They do not use
wall-clock time and cannot be prolonged by a regressing caller clock. A normal kick
returns the peer to the lobby; a temporary or platform ban is fatal for that
admission attempt.

Disconnect packets contain stable codes only: malformed traffic, rate limiting,
invalid input, ownership failure, or authentication failure. User-facing text is
localized on the client from the code and bounded detail ID. Authentication ticket
bytes, relay credentials, IP addresses, and arbitrary attacker-controlled strings
must never enter logs.

## Ban bridge

`BanProvider` is the authority integration seam for an operator or publisher ban
service. `LocalBanRegistry` is a fixed 256-entry process-local implementation for
private/listen operation and tests. It supports expiring and permanent records,
bounded offense counts, explicit removal, and expiry purging. Capacity exhaustion
fails closed for the attempted record and must be surfaced to operations; it never
allocates an unbounded fallback.

`AuthorityPeerHub<S, E, B>` owns one `PeerSecurityGuard` per authenticated live
link and one `B: BanProvider` per match. The default constructor selects
`LocalBanRegistry`; service deployments inject their provider with
`new_with_ban_provider`. Both initial attach and reconnect consult the provider
before allocating a runtime or reclaim reservation. `revoke_authentication` and
`enforce_platform_ban` are the stable callback boundary for the platform layer.

The hub classifies bounded runtime metric deltas (malformed/decode/direction,
receive-budget, queue, reliable-window, and conflicting-message failures), session
phase failures, spoofed peer/seat identity, and invalid input. A kick immediately
isolates the peer logically, starts reconnect/substitute policy, purges non-Control
outbound work, and enters a bounded `Closing` phase. Already-sequenced Control
predecessors are preserved so the reliable-ordered stream cannot acquire a gap.
The stable-code `Disconnect` is tracked by exact channel and sequence until its ACK,
reliable retry exhaustion, transport loss, or the 120-tick close deadline. A full
Control queue defers admission of the terminal behind those predecessors without
restoring gameplay eligibility. A fresh same-identity reconnect may retire the old
physical generation early, but stale callbacks and packets from it cannot affect
the replacement. Per-peer guards, close state, the local registry, and the audit
ring all have fixed capacities; no packet payload or authentication material is
retained.

The client accepts a typed terminal only from its authenticated authority and only
for the exact active match. `NetworkRuntime` queues the ACK before publishing that
message to the client protocol. The client worker then atomically publishes the
bounded payload together with its local generation/progress context. The native
application revalidates role, match, and generation, retains the first valid
terminal, and derives all screen/actions from its four-valued retry disposition.
The payload's tick and detail code remain diagnostics, never UI text or reconnect
state. Client-authored Disconnect fields are not authority decisions and are
ignored when the authority observes a raw close request.

Dropping the application-facing Steam endpoint starts an outbound-only drain rather
than closing the native socket immediately. Already-queued AFC datagrams—including
the terminal ACK—continue through the normal bounded send budget; backend receive is
disabled. The socket closes after 50 ms with no pending outbound work, at a hard
250 ms cap under persistent backpressure, or immediately on a backend failure.

Steam VAC/publisher/auth-ticket responses are inputs to this policy, not substitutes
for AFC protocol validation. A revoked authentication session disconnects the
matching peer and invalidates reconnect admission. Ranked or reward-bearing modes
remain dedicated-authority-only and require the shipping operator ban provider.

## Required acceptance scenarios

- malformed envelope, codec, direction, and over-limit datagram traffic;
- sustained receive-budget and queue flooding;
- reliable reorder-window abuse and conflicting reliable retransmissions;
- invalid axes, button masks, sequences, ticks, seats, peer IDs, and match IDs;
- spoofed reconnect identity and replayed authentication admission;
- forged future or wrong-hash state-baseline acknowledgements;
- cycling hashes and increasing ticks after acknowledgement history expiry, proving
  the verified baseline and active repair count cannot be advanced by the peer;
- repeated otherwise-valid peer repair requests beyond the per-peer cooldown and
  rolling-window budget, including first-denial retention and eventual isolation;
- authentication revocation during countdown and fighting;
- reliable terminal ACK, duplicate ACK, wrong ACK, retry exhaustion, Control-queue
  saturation/deferred admission, timeout, and same-identity generation replacement;
- Steam endpoint drop with a queued terminal ACK, permanent send backpressure,
  outbound quiet completion, and proof that no backend receive occurs while draining;
- temporary-ban expiry, permanent platform ban, and registry-capacity behavior;
- isolation proof: an abusive connection is removed while other peers complete the
  same match with canonical hash parity;
- privacy audit proving tickets and private transport credentials are absent from
  diagnostics and replay archives.

The shipping gate also requires actionable counters for violations, warnings,
kicks, bans, queue high-water marks, and authentication rejection reasons, keyed by
match/peer/seat/tick where applicable.
