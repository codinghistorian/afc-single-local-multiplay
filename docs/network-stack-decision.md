# Network Stack Decision: Lightyear 0.26.4 and Steam

- Status: **implemented AFC UDP/loopback and custom auth-gated Steam listen path; live Steam validation pending**
- Decision date: 2026-07-23
- Applies to: native online multiplayer
- Architecture authority: [multiplayer-architecture.md](multiplayer-architecture.md)

## Decision summary

Use exactly Lightyear `0.26.4` with Bevy `0.18.1` as the first WP6 networking
substrate. Lightyear may provide native UDP/netcode, bounded crossbeam transport,
connection plumbing, and the five delivery channels defined below. It does not own
AFC's simulation, canonical tick, identities, input history, prediction, rollback,
snapshots, replay, authentication, or results.

This remains a narrow compatibility decision, not a blanket framework adoption.
All Lightyear types stay behind the adapter, and AFC retains the in-process and
ordinary UDP transports. The deterministic WP6 laboratory and production-live
composition now exist in repository automation; the custom Steam listen path is
wired above them and still requires the Stage 5 external account/machine gate.

> **Shipping constraint:** Lightyear 0.26.4's stock Steam adapter remains approved
> only for a compatibility spike. It is **not** the production listen transport.
> AFC's custom auth-gated `steam_transport.rs` closes the client/listen admission
> boundary; neither path supplies hosted Steam dedicated SDR.

## Exact dependency and feature policy

Pin the compatibility pair exactly rather than accepting semver-compatible updates:

```toml
bevy = { version = "=0.18.1", ... }
lightyear = { version = "=0.26.4", default-features = false, optional = true, features = [
    "std",
    "client",
    "server",
    "udp",
    "netcode",
    "crossbeam",
] }
```

The native network feature enables the optional Lightyear dependency. A separate
`steam-net` feature extends that native set with `lightyear/steam` and the exact
optional `steamworks = 0.12.2` client binding used by the explicit
[Steam platform foundation](steam-platform-foundation.md) and
[auth-gated gameplay adapter](steam-gameplay-transport.md). Steam support must not
become a default feature, and neither Lightyear nor Steam dependencies may enter
the browser build or the simulation crate.

Do not enable these Lightyear features for the accepted integration:

- `replication`
- `prediction`
- `interpolation`
- `deterministic`
- `input_native`
- `input_bei`
- `leafwing`
- `metrics`
- `debug`

In Lightyear 0.26.4, `metrics` unexpectedly activates `replication`, so it is also
excluded. AFC must expose its own connection, packet, queue, rollback, and desync
metrics. Any future feature addition requires a new dependency-tree review and the
same compatibility gates; it must not arrive transitively without review.

The lockfile is part of the decision. CI should check that:

- the resolved versions remain Lightyear `0.26.4` and Bevy `0.18.1`;
- there is one compatible Bevy version in the native dependency graph;
- none of the rejected Lightyear features are active; and
- builds without native networking remain free of Lightyear and Steam code.

## Ownership boundary

Lightyear is an adapter inside `afc_net`, not the multiplayer architecture.

| Concern | Owner |
| --- | --- |
| UDP/netcode and connection I/O | Lightyear behind `afc_net` |
| Delivery semantics and channel dispatch | Lightyear behind `afc_net` |
| In-process crossbeam I/O | Lightyear primitive, configured and bounded by AFC |
| Session lifecycle and compatibility policy | AFC |
| Canonical `SimTick` | AFC |
| `FighterId`, `SeatId`, `PeerId`, `SimEntityId`, `MatchId`, `SimEventId` | AFC |
| Peer-to-seat ownership and input validation | AFC authority |
| Bounded `InputBatch` and redundant input history | AFC protocol |
| Accepted input history and deadline policy | AFC authority |
| Canonical snapshots, hashes, and replay | AFC simulation/protocol |
| Full-world prediction, comparison, rollback, and hard resync | AFC client/simulation |
| Deterministic network fault injection | AFC network laboratory |
| Steam identity, authentication, ownership, and bans | AFC platform/authority boundary |
| Canonical result and progression confirmation | AFC authority |

The AFC canonical tick is a protocol/simulation type with a `u32` or `u64` range.
The final width is an AFC protocol decision. Lightyear's internal `u16` `Tick` may be
used only for adapter-local bookkeeping and must never become the canonical match or
replay clock. Every authoritative AFC message that needs time carries or resolves to
an AFC `SimTick`, with explicit wrap-safe conversion tests at the adapter boundary.

Do not expose Lightyear components, replication groups, input types, tick types, or
messages from `afc_net`. `afc_sim` and `afc_protocol` must compile without importing
Lightyear. Replacing Lightyear must require changes only to adapters and integration
tests, not to gameplay state or wire semantics.

## Wire codec and packet requirements

Treat every packet as hostile, including packets delivered by an authenticated
transport.

- Configure the Lightyear packet cap to **1,200 bytes** and keep normal Input and
  State packets under that cap without relying on fragmentation.
- Give every variable-size protocol field a compile-time maximum and reject an
  over-limit count before allocating or reading its elements.
- Prefer fixed-capacity arrays plus an explicit validated count for seat inputs,
  redundant frames, acknowledgement ranges, deltas, and manifest collections.
- Do not register messages containing unconstrained `Vec`, `String`, maps, or nested
  dynamic collections through the default `register_message` bincode path. Bincode
  can allocate from an attacker-controlled encoded length before AFC business
  validation runs.
- Where a fixed representation is not practical, install custom Lightyear
  `SerializeFns` that read and validate the length first, enforce a total decode
  budget, and only then allocate.
- Validate protocol version, message kind, IDs, seat masks, tick windows, enum tags,
  collection counts, snapshot sizes, decompression output, and per-peer rates.
- Reject unknown incompatible versions and malformed messages without panicking.
  Repeated violations use the authority's explicit disconnect policy.
- Bound all send queues, receive queues, accepted-input history, snapshot history,
  and reassembly buffers. Queue-full behavior must be observable and explicit; it
  must never grow memory without limit.

Codec tests must round-trip every message and prove rejection at each maximum plus
one, including nested lengths and truncated inputs. Fuzz/property tests must include
arbitrary bytes and valid-message mutation. A successful decode is not sufficient;
the decoded message must also pass AFC semantic validation before it can reach the
authority.

## Channel contract

Register exactly five AFC channels. Delivery configuration is part of the wire
protocol and must be identical on client and authority.

| Channel | Lightyear delivery | Direction | AFC contents |
| --- | --- | --- | --- |
| Control | Ordered reliable | Bidirectional | Handshake, manifest, readiness, loading, rematch, kick, disconnect |
| Input | Sequenced unreliable | Bidirectional | Bounded recent input batches, redundancy, relayed canonical human and bot inputs |
| State | Sequenced unreliable | Authority to client | Authoritative tick, processed-input acknowledgements, hashes, latest-wins deltas; target cadence about 50 ms |
| Resync | Unordered reliable | Authority to client | Initial, reconnect, and hard-correction full snapshots |
| Result | Ordered reliable | Authority to client | Final canonical result and confirmed statistics |

Do not move high-frequency Input or State traffic onto reliable ordered delivery.
Resync is unordered reliable because only complete identified snapshots matter; a
newer transfer must not wait behind an older ordered snapshot. Each message includes
the AFC identifiers and version data needed to reject stale or cross-match traffic.

## Authority process and loopback

There is one authority implementation for offline, listen, and dedicated modes. Run
it in a separate headless Bevy `App` on its own thread with a fixed-tick schedule. It
must not share the rendering client's ECS world, schedules, resources, or wall-clock
frame cadence. A render stall therefore cannot stall or stretch authority ticks.

The normal host client connects to that authority through the same AFC session and
protocol boundary as a remote client. For optimized in-process transport:

- create both directions from explicitly bounded crossbeam channels;
- construct each endpoint with `CrossbeamIo::new(sender, receiver)`;
- set capacities from documented AFC protocol constants;
- use non-blocking queue operations and define overflow/disconnect behavior; and
- retain codec round-trip tests even if the runtime loopback path passes typed
  messages without serialization.

Do not use Lightyear's unbounded `CrossbeamIo::new_pair` convenience or the stock
`HostClient` path. An unbounded queue couples memory growth to a slow or stalled
consumer and violates the soak and malformed-traffic gates.

The listen authority follows the same input delay, validation, snapshot, reconnect,
and result path as a dedicated authority. Host seats receive no local shortcut. The
listen process remains user-controlled, so its matches are explicitly unranked,
host loss is a no-contest, and it cannot issue trusted competitive rewards.

## UDP laboratory gate

WP6 had to succeed over ordinary UDP before the Steam transport could be accepted.
Keep the deterministic AFC fault layer at a transport-independent boundary so the
same packet stream can be subjected to latency, jitter, loss, duplication, reorder,
bandwidth caps, burst delivery, and disconnects with a recorded seed.

The minimum gate is:

| Test | Required evidence |
| --- | --- |
| `NetLoopback4` | Four clients for ten minutes; no confirmed mismatch, leak, or queue growth |
| `NetTypical4` | 100 ms RTT, 20 ms jitter, 1% loss; match completes inside normal rollback limits |
| `NetDegraded4` | 150 ms RTT, 30 ms jitter, 3% loss; useful quality warning and no authority divergence |
| `RollbackStorm` | Repeated near-limit late input; bounded history and documented tick/frame cost |
| Malformed bounded codec | Invalid lengths, IDs, masks, ticks, rates, and arbitrary bytes cannot panic or allocate without bound |

Also prove multiple local seats on one connection, snapshot restore, hard resync,
reconnect ownership, final result idempotency, packet size, bandwidth, and queue
high-water metrics. These tests remain a permanent gate for the Steam path and must
pass with the in-process and UDP adapters on every release candidate.

## Steam adapter finding and constraints

The stock Lightyear 0.26.4 `steam` feature compiles for a desktop-client,
listen-style P2P spike. That is the extent of this approval.

The audited stock path has the following production blockers:

- `add_steam_resources` assumes a logged-in Steam desktop client. It is not a
  headless Steam GameServer bootstrap path.
- Incoming Steam `SessionRequest` callbacks are automatically accepted. This occurs
  without AFC's lobby membership, connection intent, or authority admission policy.
- The adapter does not validate Steam authentication tickets, AppID ownership, or
  ban results before accepting the connection or assigning seats.
- Its `aeronet_steam 0.19.1` dependency documents `SessionConfig` as broken and warns
  that the zero-length receive path is unsound. Treat both as release blockers until
  patched and covered by regression tests.
- It exposes no Steam GameServer hosted-dedicated SDR API. Therefore it cannot prove
  the architecture's headless dedicated-authority path.

Consequently:

1. Keep the stock adapter behind `steam-net` and label all use spike-only;
   production listen play uses the custom Steamworks transport.
2. Do not ship automatic session acceptance. Gate admission on the expected lobby or
   connection token, authenticated Steam identity, AppID ownership, protocol/build
   compatibility, bans where enabled, and available seat ownership.
3. Do not promote the stock adapter unless it is patched or forked for the
   receive/configuration issues, pinned, and regression-tested. The accepted custom
   listen path does not depend on that promotion.
4. Implement lobby, invites, rich presence, callback ownership, authentication
   tickets, ownership checks, and errors as explicit Steam platform services; the
   Lightyear transport does not satisfy them.
5. Build a custom Steam GameServer transport for a hosted dedicated authority and
   SDR. It must implement the same `afc_net` transport contract as UDP and loopback.
6. Keep UDP and in-process transports permanently available for deterministic CI and
   diagnosis.

A successful listen P2P exchange does not close the authentication or dedicated-SDR
gaps. Ranked, leaderboard, tournament, reward-bearing, or trusted-progression modes
remain forbidden on a listen authority and cannot ship until the custom dedicated
Steam path passes the same protocol and fault gates.

## Staged compatibility and release tests

Advance only when every earlier stage is green.

### Stage 1: dependency and feature firewall

- Compile native client, headless authority, networking-disabled, and `steam-net`
  configurations with the exact pins.
- Inspect the dependency feature tree and fail CI if a rejected feature becomes
  active or a second incompatible Bevy version appears.
- Verify `afc_sim` and `afc_protocol` have no Lightyear or Steam dependency.

### Stage 2: protocol and bounded codec

- Register all five channels with identical endpoints and test their exact delivery
  and direction contracts.
- Round-trip all messages at zero, normal, maximum, maximum-plus-one, truncated, and
  incompatible-version boundaries.
- Run arbitrary-byte and mutated-valid-message tests under memory and packet caps.
- Assert no accepted high-frequency packet exceeds 1,200 bytes.

### Stage 3: separate authority and bounded loopback

- Run the authority as a distinct headless `App` and thread.
- Connect four clients, including multiple seats on one connection, through bounded
  `CrossbeamIo::new` endpoints.
- Stall client rendering and prove authority cadence, queue bounds, accepted input
  history, hashes, snapshots, resync, and result confirmation remain correct.
- Pass `NetLoopback4` and the reconnect/ownership fixtures.

### Stage 4: UDP and deterministic fault laboratory

- Run `NetTypical4`, `NetDegraded4`, `RollbackStorm`, disconnect/reconnect,
  version-mismatch, malformed-traffic, and bandwidth-cap scenarios over UDP.
- Compare final hashes and results with the same recorded input tape over loopback.
- Record packet size, bytes per channel, queue high-water marks, rollback depth/cost,
  resync count, and rejected-message metrics.

Only completion of this stage permits WP7 Steam work.

### Stage 5: production Steam listen-host validation

- Use two real Steam accounts and machines to exercise lobby membership, invitation,
  connection, authenticated admission, multiple local seats, play, rematch,
  host-loss no-contest, and return-to-lobby.
- Keep this environment-dependent test ignored in ordinary CI and make it a named
  manual release gate with captured logs and hashes.
- Confirm the Steam run produces the same canonical results as the loopback and UDP
  input tape.

Passing this stage approves only AFC's custom auth-gated listen path for explicitly
unranked play after security review; it does not approve the stock adapter or
dedicated play.

### Stage 6: dedicated Steam GameServer and SDR

- Bootstrap without a Steam desktop client, authenticate every connecting identity,
  validate ownership and bans, and assign seats only after admission succeeds.
- Prove hosted-dedicated SDR connectivity across intended deployment regions,
  reconnect, server loss, rate limits, operational metrics, and clean shutdown.
- Repeat the UDP fault, codec, soak, hash, result, and performance gates on release
  builds.

This stage, plus the WP8 operational and security gates, is required before any
ranked or trusted-reward mode can ship.

## Conditions for revisiting the decision

Re-audit the adapter rather than silently upgrading when Lightyear, Bevy,
`aeronet_steam`, or the Steamworks binding changes. Replace only the `afc_net`
adapter if any of these conditions cannot be met:

- fixed-tick authority cadence is coupled to the client render app;
- bounded codecs or queues cannot be enforced;
- the five channel contracts cannot be represented without head-of-line blocking;
- whole-world AFC snapshots and rollback cannot remain framework-independent;
- authenticated listen admission cannot occur before session acceptance/seat
  assignment; or
- a proper Steam GameServer SDR path cannot be implemented and operated.

The preserved AFC simulation, protocol, replay, loopback, UDP laboratory, and test
tapes are the fallback boundary. They must remain valid even if Lightyear's adapter
is replaced.
