# Multiplayer Architecture and Delivery Plan

- Status: Proposed implementation specification
- Last updated: 2026-07-23
- Target platform: Steam native client, with local/offline play retained
- Initial match size: four fighter slots

This document is the implementation authority for converting Animal Fighter Club
from local multiplayer to online multiplayer. It defines the target architecture,
the required simulation invariants, the network protocol boundaries, the delivery
order, and the acceptance gates for each work package.

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

## Current-state assessment

The local game has several qualities that make rollback feasible:

- Four stable fighter slots already exist.
- Movement and collision use custom game code rather than a third-party rigid-body
  simulation.
- Combat is already divided into recognizable input, action, movement, impact,
  item, and respawn stages.
- Arena definitions contain the static collision and gameplay data needed by a
  headless simulation.
- The total combat state is small enough to snapshot in full.

The following gaps must be closed before networking gameplay:

1. `docs/architecture.md` describes fixed-step gameplay, but authoritative systems
   are currently registered in Bevy's frame-rate `Update` schedule.
2. Gameplay timers and motion consume variable `Time::delta_secs()` values.
3. Gameplay state contains local Bevy `Entity` references.
4. Query iteration and command application are not a documented canonical order.
5. Gameplay systems directly spawn visual effects and enqueue audio/camera output.
6. Bot decisions consume elapsed wall-clock time and transcendental functions.
7. `replay_seed` is stored, but gameplay does not yet consume explicit seeded RNG
   streams.
8. The active arena is process-global, preventing isolated matches or an embedded
   client and authority from owning different worlds.
9. There is no canonical snapshot, tick hash, input tape, or replay fixture.
10. There is no headless authority binary or multiplayer protocol.

These gaps are migration requirements, not reasons to discard the existing combat
implementation.

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
- Whether the match timer advances during hitstop must be defined once and preserved
  by a fixture. The initial policy should match current local behavior.
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
  sequence: wrapping sequence number
```

The held-button set represents action-level controls such as aim/grab, light, heavy,
jump, guard, ultimate, and special. Dash may be represented as an action after local
double-tap recognition only if the recognition algorithm is identical for local and
network play. Prefer moving all gameplay-relevant gesture recognition into the
tick-based input interpreter.

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
| Match-wide input delay | 2 ticks, tunable between rounds from 1 to 4 |
| Client snapshot history | At least 32 ticks |
| Maximum normal rollback | 12 ticks / 200 ms |
| Authoritative delta rate | Approximately 20 Hz |
| Tick-hash cadence | Every authoritative update, subject to measurement |

The settings are initial validation targets, not hard-coded protocol constants.

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
- Tick rate, input delay, rollback limit, and agreed start tick.

All peers acknowledge the exact manifest. A mismatch prevents the match from
starting.

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

Use Lightyear 0.26.x as the first networking-framework candidate because it matches
Bevy 0.18 and exposes Steam transport, channels, input buffering, time
synchronization, prediction, rollback, replication, and metrics. Pin the accepted
version and hide it behind `afc_net` interfaces. Complete the networking spike before
allowing Lightyear types to spread into gameplay modules.

The spike must prove:

- One listen authority with three remote clients over Steam transport.
- One headless dedicated authority over both ordinary UDP test transport and Steam.
- Multiple local seats on one client connection.
- Whole-match snapshot restoration and bounded rollback.
- Reconnect with the same Steam identity and seat assignment.
- Required metrics and packet fault injection.

If the Steam backend or rollback integration cannot satisfy these gates, retain the
simulation and protocol and replace only the `afc_net` adapter.

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
```

Every transition has a timeout and a user-visible failure reason. A client does not
enter `Countdown` until it has authenticated, accepted the manifest, loaded required
content, applied the initial snapshot, synchronized its clock, and acknowledged
readiness.

### Disconnect and reconnect policy

Initial policy:

- Retain the fighter slot for a configurable grace period.
- During the grace period, the authority supplies neutral input or bot takeover
  according to the selected match rules.
- A reconnecting peer must reauthenticate as the same Steam identity.
- Rejoin applies a full snapshot, recent canonical inputs, current tick, and seat
  ownership before prediction resumes.
- After the grace period, apply the mode-specific forfeit, bot replacement, or
  elimination policy.
- In a listen match, authority loss ends the match; clients do not elect a new
  mid-match authority.

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
- Grants confirmed progression or submits leaderboard results.

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

## WP0: Baselines and behavior fixtures

Objective: establish evidence that prevents the multiplayer refactor from silently
changing game feel or hot-path performance.

Tasks:

- [ ] Capture the pending `FourBotStress`, `MapCycle100`, and `Soak10Minutes`
  baselines described in [performance.md](performance.md).
- [ ] Inventory every authoritative resource, component, dynamic entity, timer,
  relationship, global, and gameplay random decision.
- [ ] Document the current system execution order and command-flush boundaries.
- [ ] Capture input/result fixtures for movement, jump, dash, combo, guard, grab,
  throw, item, special, hazard, ring-out, respawn, and match completion.
- [ ] Add fixtures for simultaneous or contested interactions.
- [ ] Define the initial policy for match timers during hitstop.
- [ ] Record current entity and allocation peaks for four-fighter stress.

Acceptance gate:

- Baselines contain measured, same-hardware results rather than pending entries.
- Each high-risk combat interaction has an expected result fixture.
- Current behavior can be compared against later fixed-tick output.
- `cargo run` and `cargo test` pass.

## WP1: Real fixed tick and input layer

Objective: make local gameplay advance on a 60 Hz tick independent of render frame
rate.

Tasks:

- [ ] Introduce a canonical `SimTick` and fixed 60 Hz schedule.
- [ ] Move authoritative match, action, movement, combat, item, hazard, and respawn
  work out of frame-rate `Update`.
- [ ] Keep device sampling, UI, camera, rendering, visual interpolation, audio, and
  effects in frame-rate schedules.
- [ ] Add an input accumulator that cannot lose a tap between fixed ticks.
- [ ] Convert the four local control paths to per-tick action frames.
- [ ] Define how a render frame containing zero or multiple simulation ticks samples
  and consumes input.
- [ ] Ensure hitstop freezes selected simulation phases without stopping network tick
  progression.
- [ ] Add render interpolation between the previous and current simulation pose.

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

- [ ] Replace authoritative Bevy `Entity` relationships with stable simulation IDs.
- [ ] Replace the process-global active arena with match-owned state.
- [ ] Replace authoritative floating-second timers with integer tick counters.
- [ ] Introduce master-seed-derived named RNG streams and route all gameplay random
  decisions through them.
- [ ] Replace bot wall-clock waves and gameplay trigonometric randomness.
- [ ] Define stable ordering for fighters, dynamic entities, contacts, contested
  pickups, grabs, simultaneous impacts, and respawns.
- [ ] Replace order-dependent hash-map iteration in gameplay paths.
- [ ] Introduce bounded dynamic-object pools and deterministic overflow policy.
- [ ] Add canonical numeric quantization and serialization order.

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

- [ ] Define the versioned canonical snapshot schema.
- [ ] Capture every state group in the simulation-state inventory.
- [ ] Implement deterministic snapshot serialization and restoration.
- [ ] Implement canonical per-tick hashing.
- [ ] Add a bounded client-style snapshot history.
- [ ] Record accepted input frames for all fighter slots.
- [ ] Build a headless replay runner that advances faster than realtime.
- [ ] Store periodic hashes and optional keyframes in replay files.
- [ ] Add snapshot size and restore-time metrics.

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

- [ ] Replace direct effect spawning in authoritative systems with ordered
  `SimEvents`.
- [ ] Assign deterministic event IDs.
- [ ] Build client presentation consumers and a bounded event-deduplication history.
- [ ] Separate simulation hitstop from presentation time scaling.
- [ ] Classify every current cue as predicted, predicted-and-deduplicated, or
  confirmed-only.
- [ ] Ensure rollback discards unconfirmed events and does not replay consumed audio.
- [ ] Route result, achievement, statistic, and progression hooks through confirmed
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

- [ ] Create the authority runner and explicit match lifecycle.
- [ ] Create a headless server entry point or mode with no render/audio dependencies.
- [ ] Add in-process transport queues implementing the protocol/session interfaces.
- [ ] Make offline and local multiplayer connect through the authority boundary.
- [ ] Support one process owning multiple local seats.
- [ ] Move bot execution to the authority and expose bot input frames.
- [ ] Add input validation and connection-to-seat ownership.
- [ ] Ensure an embedded listen authority runs independently of render cadence.
- [ ] Add full initial sync and result confirmation over loopback.

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

- [ ] Implement or integrate protocol channels over ordinary UDP.
- [ ] Complete the Lightyear compatibility spike and pin the accepted version.
- [ ] Add tick synchronization, input lead/deadline handling, redundancy, and
  processed-input acknowledgements.
- [ ] Relay canonical human and bot inputs.
- [ ] Add authoritative delta state and full resync messages.
- [ ] Implement full-world client prediction.
- [ ] Implement snapshot comparison, rollback, re-simulation, and hard resync.
- [ ] Add render-only correction smoothing.
- [ ] Add deterministic latency, jitter, loss, duplication, reorder, bandwidth, and
  disconnect injection.
- [ ] Add all observability counters listed in this document.

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

Tasks:

- [ ] Add Steam initialization and callback ownership at the platform boundary.
- [ ] Implement private, friends-only, and public lobby creation/join flows as
  required by the product plan.
- [ ] Implement invites, `+connect_lobby`, rich presence, and return-to-lobby.
- [ ] Publish build, protocol, content, authority, region, rules, arena, and seat
  metadata.
- [ ] Negotiate multiple local seats per Steam peer.
- [ ] Create listen-authority connections through Steam Networking Sockets and SDR.
- [ ] Authenticate session tickets and verify AppID ownership.
- [ ] Surface clear authentication, version, timeout, host-loss, and kick errors.
- [ ] Prove the dedicated-authority Steam path even if it is not enabled for the first
  private-play milestone.
- [ ] Keep ordinary UDP and in-process transports available for tests.

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

- [ ] Finalize reconnect grace and mode-specific disconnect/forfeit policy.
- [ ] Add minimum/maximum acceptable network-quality policies and UI indicators.
- [ ] Perform long-running Steam soak and cross-region tests.
- [ ] Complete Windows, Linux, and Steam Deck determinism verification.
- [ ] Add crash-safe replay/diagnostic capture with privacy review.
- [ ] Add server health, match, network, desync, and capacity metrics.
- [ ] Add rate limits, invalid-input policy, kick reasons, and ban integration where
  required.
- [ ] Package and deploy the headless dedicated authority if ranked or trusted
  rewards are in launch scope.
- [ ] Verify result submission is idempotent and authority-confirmed.
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

- [ ] Local, listen, and dedicated modes use one simulation and authority contract.
- [ ] Gameplay advances at 60 deterministic ticks per second.
- [ ] Identical manifests and input tapes reproduce identical confirmed hashes on all
  supported native platforms.
- [ ] Clients send inputs only; the authority decides all outcomes.
- [ ] Clients predict the full combat world and recover through bounded rollback.
- [ ] Presentation is rollback-safe and irreversible effects are confirmed once.
- [ ] Full snapshots restore every gameplay relationship and RNG stream.
- [ ] Steam identity, ownership, lobby, invite, connection, and failure flows work.
- [ ] Listen play is clearly unranked; trusted modes use dedicated authority.
- [ ] Required fault, reconnect, malformed-traffic, soak, and performance tests pass.
- [ ] Packet, bandwidth, rollback, allocation, and server-tick budgets are measured.
- [ ] Replays reproduce authority results and provide sufficient desync diagnostics.
- [ ] Every implementation code change has passed `cargo run` and `cargo test`.
