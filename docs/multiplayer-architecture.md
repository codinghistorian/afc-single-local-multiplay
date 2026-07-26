# Multiplayer Architecture and Delivery Plan

- Status: Implemented architecture; release-candidate acceptance still pending
- Last updated: 2026-07-26
- Target platform: Steam native client, with local/offline play retained
- Initial match size: four fighter slots

This document is the implementation authority for Animal Fighter Club multiplayer.
It defines the architecture, required simulation invariants, network protocol
boundaries, delivery order, and acceptance gates for each work package. The
repository now implements the first-release private/friends listen architecture;
unchecked items distinguish still-pending local measurements or exhaustive
fixture breadth from external release evidence and capabilities explicitly
deferred by product policy.

The document intentionally does not prescribe every Rust type or library call. A
task is complete only when its stated invariant and acceptance gate are satisfied,
even if the implementation differs from the examples here.

## Executive decision

Use a server-authoritative simulation with full combat-world client prediction and
bounded client rollback.

- Gameplay advances at 60 deterministic ticks per second.
- Clients send only per-tick input. They never author positions, hits, damage,
  scores, item results, or entity spawns.
- The authority produces the canonical match history.
- Every client predicts every combat-relevant entity, including all fighters,
  attacks, grabs, items, specials, projectiles, and dynamic hazards.
- A client restores an authoritative snapshot and re-simulates when its predicted
  history differs from the authority.
- Rendering, animation, audio, camera, particles, and UI observe simulation state
  but do not participate in rollback.
- Steam lobbies provide discovery and invitations. Steam Networking Sockets and
  Steam Datagram Relay provide the native gameplay connection.
- Private and friends-only matches may use an embedded listen authority. Ranked,
  leaderboard, or reward-bearing matches must use a dedicated authority.

Do not implement online play by synchronizing Bevy `Transform` components or by
accepting client-reported hit results.

## Product assumptions

The architecture is based on the following assumptions. Revisit the design before
implementation if any of them change materially.

- A match contains at most four active fighter slots.
- All combatants and interactable gameplay objects are relevant to every player.
- Matches are session-based and have a clear setup, countdown, fight, result, and
  return-to-lobby lifecycle.
- A new combatant cannot join a fight already in progress.
- A disconnected player may reclaim the same slot during a bounded grace period.
- Offline single-player and local multiplayer continue to use the same simulation
  as online matches.
- The Steam release is the primary online target. The browser build may remain
  offline/local until a separate WebTransport-backed service is justified.
- Online couch co-op is representable: one Steam peer may own multiple local seats,
  while the match still contains no more than four fighter slots.

## Goals

- Local input must feel immediate under normal internet latency.
- The authority must decide every gameplay outcome.
- The simulation must be replayable from a match manifest and input history.
- Rollback must not duplicate irreversible presentation or progression effects.
- Private listen-server play and dedicated-server play must share one authority
  implementation and one protocol.
- Network behavior must remain testable without Steam by using in-process and
  ordinary UDP test transports.
- A headless server must not load meshes, scenes, audio, fonts, windows, or UI.
- Multiplayer work must retain the performance discipline in
  [performance.md](performance.md).

## Non-goals for the first online release

- Seamless mid-match listen-host migration.
- Mid-match entry into an active fighter slot.
- Spectator mode.
- Cross-version matchmaking.
- User-authored gameplay mods in secure matchmaking.
- Large-world interest management.
- Client-authoritative movement, hits, inventory, or damage.
- Server rollback after an authoritative tick has been committed.
- Kernel-level anti-cheat.
- Browser-to-Steam networking.

## Implementation-state assessment

The migration gaps that motivated this plan are now closed in repository code:

- Canonical gameplay runs in the ordered 60 Hz `FixedUpdate` pipeline while input
  sampling and presentation remain frame driven.
- Integer tick timers, stable fighter/dynamic IDs, named seeded randomness,
  deterministic contact arbitration, and TickEnd quantization own gameplay.
- `SimPosition` owns rollback-relevant translation. Render `Transform` values are
  one-way projections and are absent from the headless authority.
- Versioned snapshots, hashes, input tapes, replays, full-world prediction,
  rollback, hard resync, and rollback-safe presentation journals are implemented.
- Offline/local, listen, and render-free authority compositions use the shared
  simulation/protocol boundary.
- AFC's bounded protocol runs over in-process and ordinary UDP test transports; the
  native build provides Steam lobby, authentication, P2P/SDR, reconnect, rematch,
  and player-facing application composition behind platform adapters.

This is not a Steam release-approval claim. Real two-machine Steam/SDR behavior,
physical controller and Steam Deck coverage, supported-OS determinism,
minimum-supported-CPU execution, external GPU captures, long cross-region soaks,
depot/AppID verification, and final sealed-candidate performance evidence remain
release gates. The complete local schema-v6 graphical timing/allocator matrix now
passes on frozen patched profiling binaries; it is not external GPU evidence or a
sealed release-candidate approval. Hosted Steam dedicated/ranked operation is
product-deferred for the first private/friends listen release. The detailed evidence
split is maintained in
[multiplayer-implementation-readiness.md](multiplayer-implementation-readiness.md).

## System overview

```text
 Steam lobby / invites / authentication
                    |
                    v
 Client A/B/C == Steam Networking Sockets + SDR == Authority
     |                    inputs only                  |
     |                                                 v
 per-frame input sampler                        canonical SimWorld
     |                                                 |
 per-tick InputFrame                          inputs / hashes / snapshots
     |                                                 |
     +----> predicted SimWorld <-----------------------+
                    |
                 SimEvents
                    |
        Bevy presentation proxies
   rendering / animation / audio / UI / camera
```

The client and authority share the simulation code. Their responsibilities differ:

| Responsibility | Client | Authority |
| --- | --- | --- |
| Sample local devices | Yes | No |
| Produce local-seat input frames | Yes | Bots only |
| Validate peer ownership and input | No | Yes |
| Run canonical simulation | No | Yes |
| Run predicted simulation | Yes | No |
| Roll back after correction | Yes | No |
| Render and play audio | Yes | No |
| Confirm results and progression | No | Yes |

## Target ownership boundaries

The target codebase should expose these logical packages. They may begin as modules
inside the existing crate and move into workspace crates after their interfaces are
stable. Avoid a big-bang file move before behavior is covered by fixtures.

```text
afc_sim
  Deterministic match state, definitions, collision, combat, RNG, snapshots,
  event production, and step function. No rendering or platform APIs.

afc_protocol
  Versioned wire identities, inputs, manifests, snapshots, control messages,
  channel definitions, serialization, and content hashes.

afc_net
  Session state machine, Lightyear integration, transport abstraction, fault
  simulation, Steam connection adapter, and connection metrics.

afc_server
  Headless authority runner, authentication, input validation, match ownership,
  result confirmation, and optional dedicated-server process entry point.

ffc-prototype client
  Device input, menus, Steam lobby UI, predicted match driver, visual proxies,
  correction smoothing, HUD, camera, effects, and audio.
```

`afc_sim` must not import client presentation types. `afc_protocol` must not contain
Bevy render entities, asset handles, raw input-device identifiers, or Steam API
objects.

## Simulation contract

The conceptual public operation is:

```text
step(world, tick, inputs_for_all_slots) -> ordered SimEvents
```

The operation must be deterministic for identical initial state, content, build,
tick, and inputs. It must not read wall-clock time, frame time, operating-system
state, network state, or presentation state.

### Canonical simulation state

| State group | Required contents |
| --- | --- |
| Match | Phase, phase ticks, rules, teams, stocks, arena ID, active slots, result state |
| Clock | Current simulation tick, remaining match ticks, hitstop ticks |
| Fighters | Position, velocity, facing, grounded state, stats, action timeline, reactions, cooldowns, statuses and loadout |
| Relationships | Held item, grab holder/victim, ultimate owner/target, last credited attacker |
| Dynamic objects | Items, hitboxes, projectiles, specials, character skills and dynamic arena devices |
| Collision state | Dynamic collider state, pipe transit state, per-target hazard cooldowns |
| Randomness | One master seed, derived subsystem RNG state and consumption counters |
| Allocation | Stable simulation-ID pool generations and free lists |
| Match statistics | Canonical result-relevant counters and per-fighter totals |

Static arena geometry, move definitions, character definitions, styles, equipment,
and tuning are identified through IDs and a gameplay-content hash. They are not
duplicated in every snapshot.

### State that must remain outside the simulation

- Bevy `Entity`, `Transform`, `GlobalTransform`, `Handle`, `SceneRoot`, mesh, or
  material values.
- Render interpolation history.
- Camera position, camera shake, screen filters, controller rumble, and hit-flash
  presentation.
- Particle and audio entities.
- HUD strings and layout.
- Steam identities, lobby handles, socket handles, and authentication tickets.
- Raw keyboard, mouse, or gamepad codes.

### Stable identities

Use compact stable identifiers:

- `FighterId`: fixed slot in `0..4`.
- `SeatId`: local seat owned by a connection.
- `PeerId`: protocol identity associated with a connection; Steam IDs are mapped at
  the platform boundary.
- `SimEntityId`: kind or pool plus index and generation for dynamic objects.
- `MatchId`: unique session instance.
- `SimEventId`: tick, source simulation ID, and deterministic ordinal.

Reusing a dynamic-object slot must increment its generation. Stale relationships
must fail closed rather than targeting a newly allocated object.

### Tick and time policy

- Simulation frequency: 60 Hz.
- Every gameplay duration is stored as an integer number of ticks.
- Millisecond-authored content is converted to ticks once during content loading.
- Conversion policy must be centralized and covered by tests; active windows must
  never round down to zero ticks.
- Network tick continues during hitstop. A hitstop counter freezes the explicitly
  selected gameplay phases while input and network histories continue advancing.
- The match and network clocks advance during hitstop. `Hitstop::remaining_ticks`
  decrements once at the start of each simulation step; phase freeze checks observe
  the post-decrement value. This policy is frozen by fixed-tick fixtures.
- Presentation may run at any frame rate and interpolates between simulation poses.

### Numeric determinism policy

The first implementation must:

- Remove wall-clock and transcendental decisions from authoritative gameplay.
- Use deterministic integer RNG.
- Quantize positions, velocities, facing, health, stamina, and other continuous
  authoritative values at documented phase boundaries.
- Hash the quantized representation, not arbitrary memory bytes.
- Avoid approximate equality in canonical state comparison.
- Run cross-platform input tapes before an online alpha.

The initial implementation may retain `f32` calculations behind a `SimScalar`
boundary if canonical quantization produces identical Windows, Linux, and Steam Deck
hashes. If those tests diverge, authoritative arithmetic must move to fixed-point or
another proven deterministic representation before online release.

### Randomness policy

Derive named streams from the match seed and subsystem identity, for example:

- Bot decisions per fighter.
- Item rewards and respawns.
- Arena hazards and devices.
- Character-specific random gameplay.

Cosmetic randomness must either remain outside the simulation or consume a separate
presentation seed. Adding a cosmetic effect must never change gameplay RNG
consumption. Every gameplay RNG stream and counter is part of a snapshot.

### Canonical execution pipeline

The exact ordering is a gameplay decision and must be frozen with fixtures. Use the
following initial phase model:

1. Ingest one complete input frame per active slot.
2. Derive pressed and released edges.
3. Advance match, status, cooldown, and action counters permitted during the tick.
4. Interpret actions, item commands, specials, guards, grabs, and buffered inputs.
5. Apply fighter movement and static arena collision.
6. Apply fighter-to-fighter separation in stable fighter order.
7. Advance dynamic items, hitboxes, specials, skills, and arena devices.
8. Collect potential contacts without mutating targets.
9. Sort contacts by an explicit key and resolve impacts, trades, grabs, and throws.
10. Resolve ring-outs, deaths, respawns, stock changes, and match completion.
11. Emit ordered simulation events.
12. Quantize canonical state and calculate the tick hash.

Do not depend on ECS query order, task scheduling order, hash-map iteration, or Bevy
command-flush order. If an ECS remains inside `afc_sim`, systems must materialize and
sort stable IDs before order-dependent work.

## Input model

The frame-rate input layer samples devices and accumulates transitions until the next
simulation tick. A fast press and release between ticks must not be lost.

A conceptual input frame contains:

```text
InputFrame
  tick: SimTick
  seat: SeatId
  movement_x: signed quantized axis
  movement_y: signed quantized axis
  held_buttons: bit set
  pressed_buttons: bit set
  released_buttons: bit set
  sequence: wrapping sequence number
```

The held-button set represents action-level controls such as aim/grab, light, heavy,
jump, guard, ultimate, and special. Dash may be represented as an action after local
double-tap recognition only if the recognition algorithm is identical for local and
network play. Prefer moving all gameplay-relevant gesture recognition into the
tick-based input interpreter.

Simulation v5 defines the shared offline/online `AIM_GRAB` compiler contract. A
held button is aim-only. Releasing it at or before the inclusive five-tick grace
boundary emits exactly one grab pulse; holding it through tick five cancels that
pending pulse, and a later release does nothing. A complete press/release
accumulated between fixed ticks emits immediately. Ultimate and guard recognition
run first and cannot co-emit grab. A light/heavy edge on a grab-release tick is
staged with its ordinary grace, while a complete same-tick tap can co-emit with an
already-expiring solo attack. `LocalTickInputState` and
`local_tick_to_network_input` are the single compiler used by rendered offline,
listen, and remote-client paths.

Never send raw `KeyCode`, gamepad button, or Steam Input action handles. Device
binding remains a local concern.

### Connection and seat ownership

- A connection owns one or more `SeatId` values negotiated before countdown.
- A seat maps to exactly one `FighterId` for the active match.
- The authority rejects input for seats not owned by the sending connection.
- The total occupied fighter slots cannot exceed four.
- Bot seats are owned by the authority.
- Reconnection may reclaim only the previously assigned seats and Steam identity.

### Missing and invalid input

The authority uses a fixed input deadline. At the deadline:

- A missing frame repeats continuous held movement and held-button state from the
  last accepted frame.
- Repeating a frame cannot create another rising-edge action.
- Inputs received after a committed tick are ignored.
- Inputs too far in the future, too old, duplicated outside the sequence window, or
  for an unowned seat are rejected and counted.
- Repeated abuse triggers a rate-limited warning and then disconnect.

The authority does not roll back committed match history.

## Prediction and rollback

Clients predict the complete combat simulation rather than only their local fighter.
This is required because remote movement immediately affects hit contact, grabs,
body separation, knockback, items, hazards, ring-outs, and camera framing.

Recommended starting configuration:

| Setting | Initial value |
| --- | --- |
| Simulation/input rate | 60 Hz |
| Input redundancy | Previous 4 to 6 frames |
| Match-wide input delay | Authority-selected before commit; 2 ticks on low-latency links, bounded from 2 to 6 |
| Client snapshot history | At least 32 ticks |
| Maximum normal rollback | 12 ticks / 200 ms |
| Authoritative delta rate | Approximately 20 Hz |
| Tick-hash cadence | Every authoritative update, subject to measurement |

The authority selects one immutable delay only after every authenticated remote
has at least 20 valid samples in its connection-generation-specific, 32-sample
rolling RTT window. Unknown Steam ping readings are skipped rather than treated
as zero. The authority uses the worst nearest-rank p95 across those peers. For a
conservative high-percentile RTT budget, the starting policy is
`clamp(ceil(RTT_ms * 60 / 2000) + 1, 2, 6)`: one half-RTT of transit plus one
scheduling tick. The authority rejects a start whose half-RTT plus selected delay
cannot fit inside the 12-tick normal-rollback envelope. Replacing a native
connection resets that peer's samples, packet loss does not independently add
latency, and the selected delay is copied into the immutable manifest and never
changes during a committed match. The settings remain validation targets that
must be confirmed by the production network matrix and field telemetry.

### Correction algorithm

When a client receives authoritative state for tick `T`:

1. Locate the predicted snapshot and canonical hash for `T`.
2. If hashes match, advance the confirmed frontier.
3. If hashes differ, replace state at `T` with the authoritative snapshot.
4. Discard unconfirmed simulation events after `T`.
5. Re-simulate `T + 1` through the current predicted tick using stored local,
   remote, and bot inputs.
6. Reconcile event identities and presentation proxies.
7. Record rollback depth, reason, state size, simulation cost, and visual correction.

If `T` is older than the retained history or correction cost exceeds the safety
limit, perform a hard resynchronization from a full snapshot. The authority continues
running; one slow client never pauses the match.

### Remote and bot input prediction

Until an input arrives, predict that a remote seat preserves its last continuous
input state without inventing a new pressed edge. The authority generates bot input
and broadcasts it through the same canonical input stream. Clients do not run bot AI.

### Hit policy

Clients never send `Hit`, `Damage`, `GrabSucceeded`, or similar outcome messages.
The authority evaluates attacks at their assigned simulation tick. Do not add a
separate FPS-style server-rewind hit system on top of this timeline.

## Simulation events and presentation

The simulation emits facts; presentation decides how to show them.

Example event categories:

- Action started or changed.
- Attack became active.
- Contact, guard, perfect guard, grab, throw, or item pickup occurred.
- Damage or stamina changed.
- Fighter entered a reaction, ring-out, respawn, or elimination state.
- Item, projectile, special, or arena device spawned or despawned.
- Match phase or result changed.

Each event has a deterministic `SimEventId`. Presentation keeps a bounded record of
consumed event IDs so rollback re-simulation does not replay the same audio or spawn
duplicate particles.

| Presentation class | Policy |
| --- | --- |
| Local movement and attack startup | Show immediately from predicted state |
| Trails, ordinary particles and modest camera response | Predict and deduplicate |
| Impact audio, hitstop presentation and strong camera shake | Predict when confidence is high; deduplicate and never replay during rollback |
| Health/stamina HUD | Show predicted value, then smooth correction |
| Score, stock loss, winner and result screen | Authority-confirmed |
| Achievements, stats, progression and leaderboard writes | Authority-confirmed only |

Simulation hitstop and visual time scaling must be separate. Presentation must not
change Bevy's global virtual time in a way that changes authoritative tick cadence.

## Snapshots, hashes, and replays

### Snapshot requirements

A snapshot must be sufficient to resume the simulation without reference to older
mutable state. It includes all canonical state, RNG streams, stable-ID allocator
state, and match counters.

Snapshots must be:

- Deterministically serialized.
- Versioned.
- Bounded in size.
- Cheap to clone or restore.
- Independent of pointer values and Bevy entity allocation.
- Usable by tests without rendering or networking.

### State hashes

Hash fields in documented canonical order. Use a fast deterministic 64-bit hash for
desync detection; it is not a security primitive. Include the simulation version and
gameplay-content hash in replay and snapshot headers.

### Replay format

A replay contains:

- Protocol and simulation version.
- Gameplay-content hash.
- Match manifest.
- Complete accepted input history for every seat, including bots.
- Periodic authoritative hashes.
- Optional periodic keyframes for seeking and recovery.
- Final authoritative result.

A replay is not required to remain playable across incompatible simulation versions.
Versioned replay migration may be added later.

## Protocol

### Match manifest

The authority commits one immutable manifest before countdown:

- Match ID.
- Protocol and simulation versions.
- Gameplay-content hash and build identifier.
- Authority kind: offline, listen, or dedicated.
- Arena, rules, teams, fighter slots, characters, styles, and equipment.
- Peer-to-seat-to-fighter ownership.
- Master gameplay seed and derived-stream scheme version.
- Tick rate, input delay, rollback limit, and earliest proposed start tick
  (`agreed_start_tick`).

All peers acknowledge the exact manifest. A mismatch prevents the match from
starting. The manifest start tick is a lower bound, not an expiring deadline. Once
every peer has loaded, applied initial sync, synchronized its clock, and declared
readiness, the authority chooses one immutable actual countdown boundary:

```text
actual_start_tick = max(manifest.agreed_start_tick,
                        authority_network_tick + countdown_lead_ticks)
```

`countdown_lead_ticks` is nonzero and bounded (120 ticks by default, 600 maximum).
The authority broadcasts the selected value in `StartMessage::Countdown`; clients
use that message—not the manifest proposal—for the countdown transition and for
mapping the authority network clock onto gameplay ticks. Loading may finish after
the proposal without failing the match.

### Channels

| Channel | Delivery | Contents |
| --- | --- | --- |
| Control | Reliable ordered | Handshake, manifest, readiness, loading, rematch, kick and disconnect |
| Input | Unreliable sequenced with redundancy | Recent input frames for owned seats and relayed canonical inputs |
| State | Unreliable latest-wins | Authoritative tick, processed-input acknowledgements, deltas and hashes |
| Resync | Reliable | Full snapshot for initial sync, reconnect or hard correction |
| Result | Reliable ordered | Final canonical result and confirmed statistics |

Do not send normal inputs or high-frequency state on a reliable ordered channel.
Loss of one packet must not block newer input or state.

### Packet and bandwidth policy

- Keep high-frequency packets below approximately 1,200 bytes even if the transport
  supports fragmentation.
- Bundle multiple seat inputs and redundant history into one packet where practical.
- Delta state against an acknowledged baseline.
- Full snapshots are reserved for initial sync, reconnect, periodic recovery if
  measurements justify it, and explicit resync.
- Every full snapshot transfer also carries a reliable, identity-bound canonical
  input tail ending at the snapshot tick. It contains at most five ticks per
  occupied seat and remains below the 1,200-byte packet ceiling.
- Every variable-length field has a protocol maximum and is validated before
  allocation.
- Serialization failure or unknown incompatible message versions fail closed.

### Versioning

Maintain separate values for:

- Wire protocol version.
- Simulation rules version.
- Gameplay-content hash.
- Replay format version.

Steam lobby filters must prevent incompatible builds from matching. Hot reload of
gameplay definitions and map editing are disabled during an online session.
Authority and prediction worlds construct character moves and combat tuning only
from validated bytes embedded in the executable; native loose files and their file
watchers belong only to the rendered developer sandbox.

The compiled v2 compatibility digests are deliberately path-aware. Build identity
hashes normalized relative paths and LF-normalized bytes for every `src/**/*.rs`
file, the Cargo manifests/lockfile, enabled features, build profile, configured
release label, and Steam App ID. Gameplay-content identity remains a narrower,
explicit source set at canonical-module granularity and includes every authored
rules asset used by simulation, including Champion's Court collision authorship.
Presentation and test modules outside that canonical source set may change the
conservative exact build identity, but never the gameplay-content hash. Inline
tests or presentation helpers that still share a canonical module intentionally
receive that module's conservative content identity until the source boundary is
split.

## Authority deployment

### Offline and local

Run the same authority and simulation through an in-process transport. Local play
must not retain an alternate gameplay path. Codec round-trip tests cover the wire
format even when the optimized runtime loopback skips serialization.

### Listen authority

- Runs on a dedicated thread independent of rendering.
- Uses the same input deadlines, validation, snapshots, and result path as a
  dedicated authority.
- The host's local seats connect through the normal session boundary.
- Host inputs receive the same configured input delay as remote inputs.
- The host remains capable of modifying its process, so the match is explicitly
  unranked and cannot grant trusted competitive rewards.
- If the host disappears mid-match, end the authority, mark the match no-contest,
  and return remaining peers to the Steam lobby.

Steam automatically choosing a new lobby owner is not simulation host migration.
A new owner may start a new authority between matches.

### Dedicated authority

- Uses the same `afc_sim`, protocol, validation, and result logic.
- Loads gameplay definitions and static collision data only.
- Does not load render or audio assets.
- Authenticates every Steam identity before assigning seats.
- Is mandatory for ranked, leaderboard, tournament, or progression-bearing queues.
- May host one or multiple isolated matches per process only after profiling proves
  that multi-match scheduling remains within the tick budget.

## Steam integration

Use Steam services at the platform boundary:

- Steam lobbies: discovery, privacy, invites, roster, member metadata, readiness,
  region, build compatibility, map/rules preview, and return-to-lobby behavior.
- Steam Networking Sockets: connection-oriented realtime gameplay transport.
- Steam Datagram Relay: NAT traversal, peer IP protection, encrypted/authenticated
  routing, and relay selection.
- Steam authentication session tickets: identity and AppID ownership validation.
- Steam rich presence: lobby and join status.
- Steam Input: device presentation and bindings; the protocol still transmits only
  action-level input frames.

Lobby chat is not the gameplay data plane.

Overlay requests are local presentation operations. Lobby/action eligibility is
validated before querying current overlay readiness, and disabled invite or
binding surfaces produce a short dismissible notice rather than a session
failure. Overlay active/deactive callbacks are latest-value state: online
simulation and networking continue while local combat input is neutralized.

The Steam lobby contract uses schema 2 and separates desired admission
(`afc_admission`) from effective joinability (`afc_open`). Effective joinability is
owner-controlled and true only while peer capacity remains, every current member
has a coherent declaration, and the aggregate accepted seat count is below the
lobby seat cap. Member declarations use a staged/committed transaction marker:
`s:<revision>` while seats/loadout are being written and
`c:<revision>:<ready>` only after the complete declaration is visible. Observers
retain each user's last accepted declaration, enforce monotonic revision
continuity, and resolve concurrent capacity pressure deterministically by lobby
owner then Steam user ID. A bad declaration is peer-scoped; immutable lobby
contract drift still fails the whole session closed.

Pre-game Steam authentication signaling uses a versioned, exact-epoch envelope.
Version 2 binds lobby, attributed sender and recipient, sender peer identity,
non-zero sender/owner declaration revisions, admission purpose, and the current
`MatchId` for reconnect. Manifest commit freezes immutable Steam-user/peer/revision
leases. Those leases authorize same-match reconnect across transient roster
callback gaps, while a fresh Initial exchange requires live coherent membership
and a ready declaration. The ready gate preserves the Lobby editing window for
the first match and every owner-authored between-match epoch; reconnect remains
independent of readiness. Old revisions and old match IDs are benign stale
messages; malformed or current-epoch identity mismatches are peer-scoped hostile
input. Ticket bytes are move-only, redacted from diagnostics, and zeroized at
every owned lifetime boundary.

Lightyear 0.26.4 is pinned as a narrow native networking compatibility adapter for
Bevy 0.18. AFC owns the wire codec, channel limits, input/history model,
prediction/rollback, snapshots, and deterministic fault laboratory. No Lightyear
type crosses into gameplay or protocol modules. The stock Lightyear Steam adapter
does not satisfy AFC's pre-admission authentication contract, so production Steam
uses the custom auth-gated transport described in
[steam-gameplay-transport.md](steam-gameplay-transport.md).

Local automated composition proves a listen authority with remote clients,
multiple seats per peer, whole-world restore and bounded rollback, reconnect, and
metrics/fault injection over in-process and ordinary UDP transports. Equivalent
real Steam/SDR behavior remains a two-machine external release gate. Hosted Steam
dedicated transport is product-deferred; the local `afc-dedicated` executable
proves only the shared render-free authority contract.

References:

- [Steam Networking](https://partner.steamgames.com/doc/features/multiplayer/networking)
- [Steam Datagram Relay](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)
- [Steam matchmaking and lobbies](https://partner.steamgames.com/doc/features/multiplayer/matchmaking)
- [Steam user authentication and ownership](https://partner.steamgames.com/doc/features/auth)
- [Steam anti-cheat integration](https://partner.steamgames.com/doc/features/anticheat/vac_integration)
- [Lightyear](https://github.com/cBournhonesque/lightyear)

## Session lifecycle

Use an explicit session state machine:

```text
Offline/Menu
  -> Lobby
  -> Connecting
  -> Authenticating
  -> ManifestAgreement
  -> Loading
  -> InitialSync
  -> Ready
  -> Countdown(start_tick)
  -> Fighting
  -> ConfirmingResult
  -> Results
  -> Lobby

Countdown(start_tick) or Fighting
  -> Reconnecting(resume_phase, retained_start_tick)
  -> Authenticating
  -> InitialSync
  -> resume_phase
```

Every transition has a timeout and a user-visible failure reason. A client does not
enter `Countdown` until it has authenticated, accepted the manifest, loaded required
content, applied the initial snapshot, synchronized its clock, and acknowledged
readiness. `Countdown(start_tick)` always stores the authority-selected boundary;
the manifest proposal is never used as a replacement after this message arrives.
A reconnect records whether it interrupted `Countdown` or `Fighting`; completing
the reconnect sync resumes that exact phase, preserving the selected countdown
boundary when applicable.

### Disconnect and reconnect policy

Initial policy:

- Retain the fighter slot for a configurable grace period.
- Once the manifest is committed, temporary Steam-lobby departure retains the
  identity-to-peer binding, seat lease, and measured connection quality. It closes
  all current and pending transport connections, authentication sessions, gameplay
  endpoints, and ticket exchanges before exposing the disconnected roster state.
  Before commitment, departure removes the provisional lease and its quality state.
- Before commitment, an inbound connection is eligible only when its Steam
  identity is in the coherent current lobby roster. After commitment, the exact
  immutable identity/seat lease is the ingress allowlist; it remains valid across
  temporary membership/cache loss so same-identity reconnect is not starved by
  callback ordering. Every reconnect still requires a fresh Steam auth admission.
  Quality-rejected identities and post-commit identities without a lease are
  excluded. Rejected attempts are closed in the native listener before they
  consume bounded pending-admission capacity or public-event capacity, and a
  successfully admitted connection is recorded as the binding's pending connection.
- Connection-close callbacks are keyed to the exact connection ID. A stale callback
  from an old transport cannot clear or disconnect its authenticated replacement.
- Same-identity overlap is never inferred from the Steam user or reconnect phase.
  Only an authenticated `ReconnectAllowed` terminal may grant one exact connected
  `SteamConnectionId` a one-shot replacement capability, and it is consumable only
  by a fresh `AdmissionPurpose::Reconnect`. Initial admission, unmarked duplicates,
  non-reconnect terminals, shutdown, quality rejection, kick, and ban paths cannot
  create an overlap. The binding retains the old draining and new active
  connection IDs separately until the old callback arrives.
- An authority-authored kick or terminal first removes the peer from gameplay and
  starts its reconnect/substitute policy, then retains the physical link in a
  bounded `Closing` phase. The reliable ordered Disconnect is tracked by exact
  channel/sequence until ACK, retry exhaustion, transport loss, or a 120-tick
  deadline. Existing Control predecessors drain first; a full Control queue defers
  the terminal without restoring peer eligibility. Non-Control outbound traffic is
  purged. A same-identity authenticated replacement may preempt that old physical
  generation without inheriting any of its packets or callbacks.
- The remote client ACKs a valid typed Disconnect before publishing it atomically
  with its worker generation and local confirmed progress. Only an exact active
  match/role/generation payload reaches the application. The first valid terminal
  controls recovery (`ReconnectAllowed`, `ReturnToLobby`, `MatchEndedNoContest`, or
  `Fatal`) and wins over the later generic socket close. Authority-provided detail
  and tick fields are diagnostic only and are never displayed or used as a
  reconnect baseline.
- On a listen authority, physical cleanup is generation-safe too. Endpoint attach
  records the exact mapping
  `(peer_id, SteamUserId, AuthorityConnectionId) -> SteamConnectionId`.
  `TerminalDrained` may mark only that mapped Steam connection as terminal. A
  stale user/peer/generation tuple is a benign no-op and cannot close or clear a
  replacement. If the native close callback arrives first, its exact connection
  fact is retained for one coordinator turn so either ordering has the same
  cleanup result.
- `PeerDisconnected` always carries its exact `SteamConnectionId`.
  `PeerAuthenticationRejected` carries `Some(connection)` for an attached
  generation and `None` only for local or pre-attach rejection. Application and
  authority detach/revocation paths compare that generation before mutation, so a
  delayed old close or rejection cannot revoke a same-identity replacement.
- After the client worker drops its Steam endpoint, the transport performs an
  outbound-only drain so the queued ACK can reach the authority: 50 ms of quiet,
  a hard 250 ms cap, and immediate close on backend failure. No new native receive
  work occurs during this drain.
- Application transitions retire the whole old match transport explicitly rather
  than relying on object destruction. Retirement immediately disables listener
  admission, public events, and native receive, then services each bounded
  outbound budget before testing its deadline. Per-link quiet/hard limits remain
  50/250 ms and the complete transport has a fixed 300 ms cap with sticky
  `Complete`, `TimedOut`, or `Faulted` outcome. Ordinary `Drop` is emergency
  cleanup and offers no delivery guarantee.
- Before graceful listen shutdown, `QuiesceAdmission` raises a monotonic match
  fence: listener admission stops, pending inbound links are rejected, undelivered
  endpoint/auth events and transport requests are removed, tickets are cancelled,
  and native authentication signaling is drained but ignored. Established
  endpoints remain send-capable for the bounded typed-terminal/ACK drain. No new
  capability may cross from the fenced runtime into a worker.
- The lobby coordinator owns a fixed queue of at most four retiring transport
  generations, so a fresh between-match transport may coexist with an old
  outbound drain without reusing its callbacks or packets. It delays cancellation
  of that generation's auth tickets, Steam authentication sessions, identity
  bindings, and final lobby leave until retirement is terminal. Retiring
  identities cannot begin fresh authentication. Queue exhaustion fails closed
  rather than growing memory.
- A verified final frame, retained result, Results status, and Completed terminal
  are one atomic client-mailbox publication. Completed keeps pumping its endpoint
  without submitting gameplay input until explicit teardown; Stop preserves the
  Completed terminal. A later transport-only close is benign after confirmed
  Results, but not during Fighting or ConfirmingResult.
- Between matches, only the listen owner advances the round epoch by publishing a
  coherent newer unready Steam member declaration. Early client Rematch/Return
  actions remain bounded intents in Results. Clients reset only after observing
  that owner epoch and acknowledge with their own newer declaration; the owner
  gates fresh Initial tickets on those per-member acknowledgements. This removes
  both client-first and owner-first authentication races without treating the
  reset as host migration.
- During the grace period, the authority supplies neutral input or bot takeover
  according to the selected match rules.
- A reconnecting peer must reauthenticate as the same Steam identity.
- Rejoin restores the previously accepted manifest and actual countdown boundary,
  then applies a full snapshot plus the transfer's canonical input tail before
  prediction resumes. It does not replay ManifestAccepted, InitialSyncApplied, or
  Ready against an authority that already has an active match.
- A replacement transport queues only its authenticated handshake initially. The
  authority sends the reconnect transfer unsolicited, and the client blocks new
  gameplay input until it has sent `ResyncApplied` and collected fresh clock-sync
  samples. The authority admits input only after the same acknowledgement and
  clock gate complete.
- The input tail is copied atomically from committed authority history and is bound
  to match ID, transfer ID, snapshot tick, and snapshot hash. Every occupied seat
  has the same contiguous range ending at the snapshot. Source, seat ownership,
  fighter identity, tick range, duplicates, and canonical padding are validated.
  Snapshot tick zero uses an explicit neutral `MissingSubstitute` seed.
- Applying the snapshot is contingent on receiving the valid tail, regardless of
  whether Begin, chunks, or tail arrived first. The newest tail frame seeds each
  seat's held input at the snapshot boundary; older frames establish committed
  coverage but are never rolled back or replayed behind that boundary.
- While a reconnect snapshot is still incomplete, ordinary committed-input and
  state replication is identity/ownership validated but discarded as bounded
  pre-baseline traffic. Replication after atomic snapshot-plus-tail application
  catches the client up or triggers the normal bounded hard-resync path.
- An in-fight `HistoryExpired` or hash-mismatch repair is client-requested. The
  client fences new local input before sending the request; only then may the
  authority capture and transfer a snapshot. Unsolicited in-fight Begin, chunk,
  or tail messages fail closed. Because Resync traffic can precede Control after
  a valid request, the client may bounded-stage matching chunks/tail before Begin.
  Transfer-scoped fixed receipt caches make late byte-identical retries idempotent
  while a same-transfer conflict still fails closed.
- Peer-authored baseline acknowledgements are accepted only when their tick is not
  ahead of authority history and, while retained, their hash exactly matches the
  authority snapshot. An expired offered acknowledgement is ignored and cannot
  replace the peer's last authority-verified baseline. If that retained baseline
  expires, authoritative hash replication causes the client to request at most one
  bounded repair after fencing input; the authority does not push a stale snapshot
  across an unknown local-input generation. Forged future/hash acknowledgements
  score and detach only that peer.
- Peer-requested repair construction has a per-roster-peer cooldown and rolling
  fixed window budget that survives connection replacement. Initial sync,
  reconnect sync, and an already coalesced in-flight repair do not consume that
  abuse budget. Cooldown/window denials are explicitly
  consumed and scored as peer-local rate violations, so the first duplicate does
  not become a forced session error while sustained abuse still reaches the normal
  isolation threshold. Repeated valid repair requests therefore cannot force
  unbounded snapshot/chunk work or interrupt healthy peers.
- After the grace period, apply the mode-specific forfeit, bot replacement, or
  elimination policy.
- In a listen match, authority loss ends the match; clients do not elect a new
  mid-match authority. If a result was already confirmed, authority loss performs
  transport/authentication teardown without replacing that result, emitting a
  second terminal event, or downgrading it to no-contest.

The exact grace duration and competitive forfeit rules are product decisions that
must be recorded before Steam matchmaking work is accepted.

## Security and trust model

The server accepts intent, not outcomes.

Validate:

- Steam identity, authentication ticket, ownership, and ban result where enabled.
- Protocol, simulation, build, and gameplay-content versions.
- Connection-to-seat ownership.
- Monotonic input sequences and bounded tick windows.
- Axis ranges and valid button masks.
- Message length and collection capacity before allocation.
- Per-channel rate limits and repeated invalid behavior.
- Match configuration and loadout legality.

The authority alone:

- Selects and commits the match manifest.
- Advances gameplay.
- Runs bots and gameplay RNG.
- Creates and destroys simulation entities.
- Calculates hits, damage, score, stock, ring-outs, and results.
- Emits the confirmed result identity and canonical final state. Only an approved
  durable sink may grant progression or submit leaderboard results, and the first
  private/friends listen release has no trusted sink.

Transport encryption does not make a listen host trustworthy. Competitive integrity
requires dedicated authority.

## Performance budgets

These are starting acceptance budgets and must be replaced or refined by measured
baselines on supported hardware.

| Metric | Initial target |
| --- | --- |
| Authoritative 60 Hz simulation step | p99 below 1 ms on minimum supported CPU |
| Maximum 12-tick client rollback burst | p99 below 4 ms |
| Steady-state simulation allocation | Zero heap allocations per tick |
| Median rollback depth | 0 to 2 ticks |
| p95 rollback depth | At most 6 ticks |
| Normal rollback cap | 12 ticks / 200 ms |
| High-frequency packet size | Below approximately 1,200 bytes |
| Client upstream average | At most 16 KiB/s |
| Client downstream average | At most 64 KiB/s |
| Confirmed-history checksum mismatches | Zero after correction |
| History and queue growth | Bounded with no monotonic growth |

Rollback performance is a frame-time problem, not merely an average simulation-cost
problem. Record normal steps, number of re-simulated ticks per render frame, total
rollback time, correction magnitude, and hard-resync count.

The existing rules in [performance.md](performance.md) apply. Before modifying a
measured hot path, capture a same-hardware baseline. Update the baseline table only
after an accepted result is reproduced.

## Observability

The bounded implementation schema, privacy constraints, dashboard contract, and
incident runbook are maintained in
[multiplayer-operations.md](multiplayer-operations.md).

Expose development and telemetry counters for:

- Local, predicted, confirmed, and authority ticks.
- Clock offset and drift.
- RTT, jitter, packet loss, duplication, and reorder.
- Input lead, lateness, substitution, rejection, and redundancy recovery.
- Snapshot full and delta sizes.
- Bytes and packets per channel in each direction.
- Confirmed hash mismatches.
- Rollback count, cause, depth, re-simulation time, and corrected fields.
- Render-space correction distance and smoothing duration.
- Hard resynchronization count and reason.
- Server simulation p50, p95, and p99.
- Queue depths, history utilization, dynamic-object pool utilization, and overflow.
- Authentication, manifest, version, timeout, kick, disconnect, and reconnect events.

Logs must include match ID, peer ID, seat ID, fighter ID, and tick where applicable.
Never log authentication ticket contents or private network credentials.

## Verification matrix

### Determinism

- Run the same input tape twice in one process and compare every tick hash.
- Compare debug and release output.
- Compare Windows, Linux, and Steam Deck output.
- Permute dynamic allocation and unrelated presentation spawn order.
- Exercise every arena, character, style, equipment item, special, dynamic item, and
  hazard.
- Verify RNG stream isolation by adding cosmetic events without changing gameplay
  hashes.
- Run at least 100,000 ticks per deterministic soak fixture.

### Network fault simulation

All networking must be testable through a deterministic fault layer supporting
latency, jitter, loss, duplication, reorder, disconnect, bandwidth caps, and burst
delivery.

Required scenarios:

| Scenario | Conditions | Required result |
| --- | --- | --- |
| `NetLoopback4` | Four clients, loopback, ten minutes | No confirmed mismatch, leak, or queue growth |
| `NetTypical4` | 100 ms RTT, 20 ms jitter, 1% loss | Match completes; rollback remains within normal cap |
| `NetDegraded4` | 150 ms RTT, 30 ms jitter, 3% loss | Graceful degradation, useful quality warning, no authority divergence |
| `RollbackStorm` | Repeated late remote input near limit | No frame budget violation beyond documented threshold |
| `Reconnect` | Disconnect and reclaim during combat | Correct identity, slot, snapshot, tick, and result |
| `AuthorityLoss` | Listen host disappears | No-contest and clean return-to-lobby flow |
| `VersionMismatch` | Protocol/content/build mismatch | Refusal before countdown with clear reason |
| `MalformedTraffic` | Invalid lengths, masks, IDs, ticks and rates | No panic or unbounded allocation; reject or disconnect |

### Gameplay behavior

- Local/offline results match the new authority path for the same manifest and input
  tape.
- Simultaneous hits, grabs, guard timing, throws, item pickup conflicts, hazard
  contacts, ring-outs, respawns, and match completion have explicit fixtures.
- Prediction never changes canonical results.
- A hard resync produces the same subsequent result as uninterrupted authority play.
- Confirmed result, achievement, and progression hooks execute once.

### Required repository validation

For every code change:

```bash
cargo run
cargo test
```

Changes to measured hot paths also follow [performance.md](performance.md). Changes
affecting the web build continue to write the complete artifact only to repository-
root `web_dist/`.

## Work-package map

| Work package | Outcome | Depends on |
| --- | --- | --- |
| WP0 | Baselines and behavior fixtures | None |
| WP1 | Real fixed tick and tick input layer | WP0 |
| WP2 | Deterministic identities, time, RNG and ordering | WP1 |
| WP3 | Canonical snapshots, hashes and replays | WP2 |
| WP4 | Simulation/presentation event boundary | WP3 |
| WP5 | Headless authority and shared local loopback | WP4 |
| WP6 | Network laboratory, prediction and rollback | WP5 |
| WP7 | Steam lobby, authentication and SDR | WP6 |
| WP8 | Production hardening and dedicated deployment | WP7 |

Do not start WP7 before WP6 passes under ordinary UDP and deterministic fault
simulation. Steam must be an adapter over a proven session and simulation model.

Checklist notation in this document is implementation-specific: `[x]` means the
production boundary exists and has local automated evidence. It does not claim
that a work package's measured, cross-platform, two-machine Steam, GPU, depot, or
other external acceptance gate has passed.

## WP0: Baselines and behavior fixtures

Objective: establish evidence that prevents the multiplayer refactor from silently
changing game feel or hot-path performance.

Tasks:

- [x] Capture the schema-v6 `FourBotStress`, `MapCycle100`, and `Soak10Minutes`
  local baselines described in [performance.md](performance.md): three timing and
  three allocator runs for each short scenario, plus one timing and one allocator
  ten-minute soak, all on the same host and frozen patched binaries. The timing
  binary SHA-256 is
  `9caaa991644f367d772e11a4f7964ec71c25f0b51d496828558b1e2aaed6e7fd`;
  the allocator binary SHA-256 is
  `54d6239ec592bf3139f24cfc120abb23ccfbd7115a22e70bec097d7920b49db6`.
  Every accepted `MapCycle100` run preloads exactly 101 supported assets, completes
  10 warm presents and 100 measured switches, observes exactly 111 present ACKs
  and 11 aligned checkpoints, and passes the aligned-tail RSS gates (range at most
  8 MiB and slope at most 2 MiB/min). Allocator runs also pass the aligned-tail live
  gates (range at most 1 MiB and slope at most 0.25 MiB/min). Per-run before/after
  host, architecture, binary-hash, and AC-power records are retained. One otherwise
  passing allocator sample that crossed from AC power to battery was rejected and
  replaced; it is not counted among the three accepted runs.
- [x] Inventory every authoritative resource, component, dynamic entity, timer,
  relationship, global, and gameplay random decision.
- [x] Document the current system execution order and command-flush boundaries.
- [x] Capture production-headless category tapes for movement, jump, dash, combo,
  guard, grab, throw, item, special, hazard, ring-out, respawn, and match
  completion. These freeze representative category paths, not every
  character/arena/move combination.
- [x] Add representative simultaneous and contested evidence through BF024/BF025
  plus focused reverse-allocation-order tests. Exhaustive authored-content
  combinations remain continuing corpus work rather than a claim of complete
  behavioral enumeration.
- [ ] Expand the representative category tapes into exhaustive
  character/arena/move interaction breadth before making any exhaustive behavior
  coverage claim.
- [x] Define the initial policy for match timers during hitstop.
- [x] Record current entity and allocation peaks for four-fighter stress in the
  schema-v6 capture results summarized by [performance.md](performance.md).

Acceptance gate:

- Baselines contain measured, same-hardware results rather than pending entries.
- Each high-risk combat interaction category has a representative expected-result
  fixture; exhaustive breadth remains explicitly tracked.
- Current behavior can be compared against later fixed-tick output.
- `cargo run` and `cargo test` pass.

## WP1: Real fixed tick and input layer

Objective: make local gameplay advance on a 60 Hz tick independent of render frame
rate.

Tasks:

- [x] Introduce a canonical `SimTick` and fixed 60 Hz schedule.
- [x] Move authoritative match, action, movement, combat, item, hazard, and respawn
  work out of frame-rate `Update`.
- [x] Keep device sampling, UI, camera, rendering, visual interpolation, audio, and
  effects in frame-rate schedules.
- [x] Add an input accumulator that cannot lose a tap between fixed ticks.
- [x] Convert the four local control paths to per-tick action frames.
- [x] Define how a render frame containing zero or multiple simulation ticks samples
  and consumes input.
- [x] Ensure hitstop freezes selected simulation phases without stopping network tick
  progression.
- [x] Add render interpolation between the previous and current simulation pose.

Acceptance gate:

- Identical input tapes produce identical results at 30, 60, 120, and uncapped render
  frame rates.
- Fast button taps are preserved.
- Fixed-tick and presentation schedules have documented ownership.
- Existing behavior fixtures either match or contain an explicitly accepted change.
- Before/after hot-path measurements are recorded.
- `cargo run` and `cargo test` pass.

## WP2: Deterministic identities, time, RNG and ordering

Objective: remove machine-local and order-dependent values from authoritative state.

Tasks:

- [x] Replace authoritative Bevy `Entity` relationships with stable simulation IDs.
- [x] Replace the process-global active arena with match-owned state.
- [x] Replace authoritative floating-second timers with integer tick counters.
- [x] Introduce master-seed-derived named RNG streams and route all gameplay random
  decisions through them.
- [x] Replace bot wall-clock waves and gameplay trigonometric randomness.
- [x] Define stable ordering for fighters, dynamic entities, contacts, contested
  pickups, grabs, simultaneous impacts, and respawns.
- [x] Replace order-dependent hash-map iteration in gameplay paths.
- [x] Introduce bounded dynamic-object pools and deterministic overflow policy.
- [x] Add canonical numeric quantization and serialization order.

Acceptance gate:

- No authoritative state stores a Bevy `Entity` or reads frame/wall-clock time.
- Reordering unrelated presentation entities cannot change a gameplay hash.
- The replay seed changes gameplay RNG and reproduces it exactly.
- Dynamic capacity exhaustion has a tested deterministic result and never allocates
  without bound.
- Same-process deterministic soaks have zero hash divergence.
- `cargo run` and `cargo test` pass.

## WP3: Canonical snapshots, hashes and replays

Objective: make the complete match restorable and replayable at any retained tick.

Tasks:

- [x] Define the versioned canonical snapshot schema.
- [x] Capture every state group in the simulation-state inventory.
- [x] Implement deterministic snapshot serialization and restoration.
- [x] Implement canonical per-tick hashing.
- [x] Add a bounded client-style snapshot history.
- [x] Record accepted input frames for all fighter slots.
- [x] Build a headless replay runner that advances faster than realtime.
- [x] Store periodic hashes and optional keyframes in replay files.
- [x] Add snapshot size and restore-time metrics.

Acceptance gate:

- Restoring tick `T` and replaying inputs produces the original hash at every later
  tick.
- A replay can reproduce complete result and telemetry without rendering.
- Snapshot and history memory remain within documented bounds.
- Cross-build and cross-platform fixtures either match exactly or trigger the
  numeric-determinism escalation before proceeding.
- `cargo run` and `cargo test` pass.

## WP4: Simulation/presentation event boundary

Objective: make rollback safe for effects, audio, camera, HUD, and progression.

Tasks:

- [x] Route authoritative effect facts through ordered `SimEvents`; retained direct
  visual sidecars are explicitly cosmetic and absent from the headless composition.
- [x] Assign deterministic event IDs.
- [x] Build client presentation consumers and a bounded event-deduplication history.
- [x] Separate simulation hitstop from presentation time scaling.
- [x] Classify every current cue as predicted, predicted-and-deduplicated, or
  confirmed-only.
- [x] Ensure rollback discards unconfirmed events and does not replay consumed audio.
- [x] Route result, achievement, statistic, and progression hooks through confirmed
  authority events.

Acceptance gate:

- Re-simulating the same tick does not duplicate particles, audio, rumble, camera
  effects, scores, results, or progression.
- Presentation can be disabled entirely while the simulation and replay tests pass.
- Visual quality settings cannot affect gameplay hashes.
- Before/after performance and allocation measurements are recorded.
- `cargo run` and `cargo test` pass.

## WP5: Headless authority and shared local loopback

Objective: run one canonical authority implementation without rendering and make all
local play use it.

Tasks:

- [x] Create the authority runner and explicit match lifecycle.
- [x] Create a headless server entry point or mode with no render/audio dependencies.
- [x] Add in-process transport queues implementing the protocol/session interfaces.
- [x] Make player-facing offline and local multiplayer connect through the authority
  boundary; the developer sandbox retains an intentional direct-debug mode.
- [x] Support one process owning multiple local seats.
- [x] Move bot execution to the authority and expose bot input frames.
- [x] Add input validation and connection-to-seat ownership.
- [x] Ensure an embedded listen authority runs independently of render cadence.
- [x] Add full initial sync and result confirmation over loopback.

Acceptance gate:

- Local play no longer has a separate gameplay path.
- The headless authority runs complete matches without loading client assets.
- Four local seats and mixed human/bot matches complete through protocol boundaries.
- Client render stalls do not change authority tick cadence.
- Replay hashes match the authority's accepted input history.
- `cargo run` and `cargo test` pass.

## WP6: Network laboratory, prediction and rollback

Objective: prove the architecture over a non-Steam network with deterministic fault
injection.

Tasks:

- [x] Implement or integrate protocol channels over ordinary UDP.
- [x] Complete the Lightyear compatibility spike and pin the accepted version.
- [x] Add tick synchronization, input lead/deadline handling, redundancy, and
  processed-input acknowledgements.
- [x] Relay canonical human and bot inputs.
- [x] Add authoritative delta state and full resync messages.
- [x] Implement full-world client prediction.
- [x] Implement snapshot comparison, rollback, re-simulation, and hard resync.
- [x] Add render-only correction smoothing.
- [x] Add deterministic latency, jitter, loss, duplication, reorder, bandwidth, and
  disconnect injection.
- [x] Add the bounded local observability counters listed in this document.

Acceptance gate:

- `NetLoopback4`, `NetTypical4`, `NetDegraded4`, and `RollbackStorm` pass.
- Normal rollbacks remain inside the depth and cost budgets.
- Confirmed results always match the authority.
- Packet and bandwidth budgets are measured and documented.
- Malformed traffic cannot panic or allocate without bound.
- `cargo run` and `cargo test` pass.

## WP7: Steam lobby, authentication and SDR

Objective: replace the test transport/session discovery with Steam-native user flow
without changing simulation behavior.

Implementation status and the exact remaining wiring/hosted-SDR boundary are
tracked in [steam-platform-foundation.md](steam-platform-foundation.md) and the
[auth-gated Steam gameplay transport](steam-gameplay-transport.md).

Tasks:

- [x] Add Steam initialization and callback ownership at the platform boundary.
- [x] Implement private and friends-only lobby creation/join flows; public discovery
  is deliberately rejected by the first-release product policy.
- [x] Implement invites, `+connect_lobby`, rich presence, and return-to-lobby.
- [x] Publish build, protocol, content, authority, region, rules, arena, and seat
  metadata.
- [x] Negotiate multiple local seats per Steam peer.
- [x] Create auth-gated listen-authority connections through Steam Networking
  Sockets/SDR; real relay selection remains an external gate.
- [x] Authenticate session tickets and verify AppID ownership at the platform
  boundary; real account callbacks remain an external gate.
- [x] Surface clear authentication, version, timeout, host-loss, and kick errors.
- [ ] Prove the dedicated-authority Steam path even if it is not enabled for the first
  private-play milestone. This is product-deferred for the listen release.
- [x] Keep ordinary UDP and in-process transports available for tests.

Acceptance gate:

- Friends can invite, launch, join, ready, play, rematch, and return to lobby.
- Incompatible clients are rejected before countdown.
- Invalid authentication is rejected according to Steam requirements.
- Listen host loss follows the documented no-contest behavior.
- Steam transport produces the same confirmed hashes and results as UDP tests.
- `cargo run` and `cargo test` pass.

## WP8: Production hardening and dedicated deployment

Objective: meet shipping reliability, security, performance, and operations gates.

Tasks:

- [x] Finalize the casual listen reconnect grace and disconnect/bot-takeover policy;
  competitive forfeit policy remains product-deferred with competitive play.
- [x] Add minimum/maximum acceptable network-quality policies and UI indicators.
- [ ] Perform long-running Steam soak and cross-region tests.
- [ ] Complete Windows, Linux, and Steam Deck determinism verification.
- [x] Add crash-safe replay/diagnostic capture with bounded privacy-safe records.
- [ ] Complete the external product privacy/consent review for release diagnostics.
- [x] Add bounded local server health, match, network, desync, and capacity metrics.
- [x] Add rate limits, invalid-input policy, kick reasons, and listen/platform-ban
  integration. A shipping operator ban provider is deferred with hosted dedicated
  service.
- [ ] Package and deploy the headless dedicated authority if ranked or trusted
  rewards are in launch scope.
- [x] Verify result submission is idempotent and authority-confirmed.
- [ ] Run final performance, memory, bandwidth, rollback, reconnect, and failure
  matrices on release builds.

Acceptance gate:

- No unresolved critical or high-severity multiplayer failure remains.
- All required network scenarios pass on release builds.
- Ten-minute and extended soaks show no memory, entity, history, or queue growth.
- Ranked/reward-bearing modes cannot use a listen authority.
- Operational dashboards and actionable failure logs exist before public rollout.
- `cargo run` and `cargo test` pass.

## Decisions required before their dependent work

These product decisions do not block WP0 through WP6, but must be recorded before
the listed milestone is accepted.

The initial-release choices are now recorded in
[multiplayer-product-policy.md](multiplayer-product-policy.md). Any later change to
those choices must update that decision record and the affected acceptance tests.

| Decision | Required before | Default recommendation |
| --- | --- | --- |
| Are ranked or trusted rewards in the first online release? | WP7 scope lock | If yes, ship dedicated authority with them |
| Reconnect grace duration | WP8 | Short casual grace; stricter competitive forfeit |
| Neutral input versus bot takeover during disconnect | WP8 | Bot takeover for casual, rules-specific competitive policy |
| Maximum supported matchmaking RTT | WP8 | Prefer under 100 ms; warn/degrade before a hard cap |
| Public lobby browser versus friends-only first | WP7 | Friends/private first, public after network telemetry is stable |
| Online couch co-op UI scope | WP7 | Preserve protocol support from the start even if UI ships later |
| Result/progression backend | WP8 | Authority-confirmed and idempotent; never trust listen-host results for ranked rewards |

## Risk register

| Risk | Consequence | Mitigation / stop condition |
| --- | --- | --- |
| Cross-platform floating-point divergence | Frequent rollback or unreproducible replays | Canonical quantization and cross-platform tapes; move authoritative arithmetic to fixed-point if hashes differ |
| Presentation remains coupled to simulation | Duplicate effects and non-repeatable rollback | Complete WP4 before network rollback |
| Whole-world snapshots are larger than expected | Bandwidth or restore spikes | Measure in WP3; bounded pools, delta compression, compact IDs and quantized fields |
| Rollback bursts exceed frame budget | Visible hitching | Allocation-free simulation, strict rollback cap, profile worst-case bursts, hard resync beyond cap |
| Lightyear integration leaks through gameplay | Difficult library upgrades or fallback | Keep it behind `afc_net`; require transport-independent tests |
| Listen-host advantage or cheating | Untrusted results | Equal configured delay for host; mark listen play unranked; dedicated authority for trusted modes |
| Host migration expands scope | Delays core multiplayer | Explicitly no mid-match migration in v1 |
| Steam work begins before simulation is stable | Hard-to-debug mixed failures | WP7 cannot start until WP6 fault tests pass |
| Existing behavior changes during extraction | Combat feel regression | WP0 fixtures and explicit acceptance of each changed outcome |
| Unbounded dynamic entity creation | Snapshot, memory and denial-of-service risk | Fixed capacities, deterministic overflow policy and utilization metrics |

## Definition of multiplayer-ready

The game is multiplayer-ready only when all of the following are true:

- [x] Local, listen, and the local dedicated smoke use one simulation and authority
  contract.
- [x] Gameplay advances at 60 deterministic ticks per second.
- [ ] Identical manifests and input tapes reproduce identical confirmed hashes on all
  supported native platforms.
- [x] Clients send inputs only; the authority decides all outcomes.
- [x] Clients predict the full combat world and recover through bounded rollback.
- [x] Presentation is rollback-safe and irreversible effects are confirmed once.
- [x] Full snapshots restore every gameplay relationship and RNG stream.
- [x] Local fake-backend application tests exercise Steam identity, ownership,
  lobby, invite, connection, reconnect, rematch, and failure state-machine
  boundaries.
- [ ] Real two-machine Steam identity, ownership, lobby, invite, authentication,
  SDR connection, teardown, and failure flows pass on licensed accounts.
- [x] Listen play is clearly unranked; trusted modes are unavailable until a
  dedicated authority exists.
- [x] Deterministic local fault, reconnect, malformed-traffic, security,
  bandwidth, bounded-queue, and production-headless soak fixtures pass in
  repository evidence.
- [x] Canonical-pose authority/rollback hot paths and protocol development-reference
  packet, bandwidth, rollback, and server-tick budgets have same-hardware or
  automated measurements.
- [x] The complete local schema-v6 graphical timing/allocation matrices and
  ten-minute plateau evidence pass on frozen patched profiling binaries. Each
  result still reports
  `external_gpu_evidence_status=required_not_collected` and
  `gpu_completion_measured=false`; the sealed-candidate repeat and external
  promotion gates remain separate.
- [ ] External GPU capture, minimum-supported-CPU budgets, supported-OS/Steam Deck
  runs, cross-region Steam soak, signed packaging, depot preview/upload, and
  promotion evidence pass for the same sealed candidate.
- [x] Replays reproduce authority results and provide bounded desync diagnostics.
- [x] Every implementation code change has been validated with the exact
  `cargo test` command and an exact `cargo run` native launch. The graphical
  run reached the Metal renderer and created the Animal Fighter Club window;
  because the normal client has no automatic exit, the verification process
  was then stopped manually. The dedicated 120-tick `cargo run` smoke also
  exited cleanly.
