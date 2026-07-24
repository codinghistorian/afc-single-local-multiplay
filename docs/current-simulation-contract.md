# Current Simulation Contract

- Status: Implemented simulation-v5 contract with historical WP0 provenance
- Historical audited source: `d33ceff65065e18d0928820892bb24bfb5c845ae`
- Current audit date: 2026-07-24
- Scope: current deterministic combat contract plus the preserved pre-WP1 inventory
- Target specification: [multiplayer-architecture.md](multiplayer-architecture.md)

This document originally froze local behavior before the fixed-tick,
stable-identity, snapshot, and rollback migration. It now records the implemented
simulation-v5 contract while retaining the original execution inventory as
migration provenance. Sections explicitly labelled **historical WP0** describe
the old source above and are not claims about the current runtime.

Current gameplay runs in Bevy's ordered 60 Hz `FixedUpdate` schedule. Device
sampling, interpolation, UI, camera, audio, and rendering remain frame driven.
The source-of-truth system matrix is
[determinism-system-audit.md](determinism-system-audit.md), and the multiplayer
composition/release evidence split is
[multiplayer-implementation-readiness.md](multiplayer-implementation-readiness.md).

## How to use this contract

The WP1-WP4 cutover must preserve every behavior identified as **preserve** below.
A behavior may change only when all of the following are recorded together:

1. the affected fixture and old result;
2. the new expected result and gameplay reason;
3. the simulation-version change that owns the new result; and
4. the reviewer who accepted the game-feel change.

Do not regenerate a golden fixture merely because a refactor changed its output.
The first frozen production-headless tape and its three-operating-system CI gate
are specified in [cross-platform-determinism.md](cross-platform-determinism.md).
First classify the mismatch as a bug, an explicitly accepted behavior change, or
previously undefined ECS ordering.

A canonical hash can also change without a gameplay-rule change when the
conservative gameplay-content source digest changes. That case follows the
content-identity-only refresh rule below: it changes compatibility identity and
therefore requires new hashes, but it does not by itself justify a simulation
version bump or authorize a different semantic result.

Inside a section labelled historical WP0, **current** and **legacy** mean the
audited pre-cutover commit above. Elsewhere, **current** means simulation version
5 in this repository. **Target** refers to the multiplayer specification.

## Simulation v5 aim/grab input addendum

The current online compatibility boundary is simulation version 5. Protocol
version 1, snapshot schema 2, replay schema 1, and the 60 Hz tick rate are
unchanged. Version 5 intentionally changes the canonical local gesture compiler:

- Holding `AIM_GRAB` is aim-only.
- Releasing at or before the inclusive five-tick grace boundary emits exactly one
  grab action. Holding through the boundary cancels the pending grab, so a later
  release cannot resurrect it.
- A press and release accumulated between fixed steps emits grab on that first
  consumed step.
- Ultimate and guard recognition retain priority and never co-emit grab.
- A light/heavy edge arriving on the grab-release step is staged under its normal
  grace. A complete accumulated grab tap emits immediately even when a prior solo
  attack expires on that step; the two action pulses may co-exist.
- Resetting a seat clears pending gesture history but preserves the session input
  sequence unless a complete session reset was requested.

The production compiler is shared by rendered offline play, listen clients, and
remote online clients. Gesture history is deliberately not snapshot state, so
versioned fixture input is compiled into immutable action frames before a
simulation or restore run. The v5 lobby handshake rejects v4 before countdown,
and v4 replay data is incompatible with v5 playback even though the replay wire
schema remains decodable for diagnostics.

## Simulation v4 deterministic-math addendum

The historical WP0 inventory below remains useful as migration provenance. The
version 4 boundary changed the numeric implementation contract without
intentionally changing the authored gameplay rules:

- Canonical `Vec2`/`Vec3` squared length and squared distance use explicit scalar
  component order.
- Canonical length, distance, and normalization use the exact direct dependency
  `libm = 0.2.16` with `force-soft-floats`; zero, subnormal-underflow,
  non-finite, and overflow inputs follow the tested fallback policy in
  `canonical_math.rs`.
- Pure range and overlap predicates compare squared distance with squared,
  non-negative authored radii. Sites that need an actual speed or radial falloff
  retain the canonical software square root.
- The Chick ultimate's sixteen relative directions come from an exact,
  fingerprinted basis. Runtime authoritative `atan2`/`sin`/`cos` is not used for
  that burst. C1 trajectory and collision lookup tables remain identified as
  simulation-v3 reference data.
- Quantized live-input conversion is part of the gameplay source digest.
  Bot decision math remains its existing integer/fixed deterministic contract.
- Presentation-only camera, pose, particle, scene-orientation, and animation math
  may continue to use glam and platform trigonometry because it cannot feed the
  canonical state.

The historical v4 lobby handshake rejected a v3 simulation identity before
admission. Replay files remain decodable for archival diagnostics, but exact
compatibility validation rejects older simulation identities before playback.
The frozen vector corpus and production-headless tape are documented in
[cross-platform-determinism.md](cross-platform-determinism.md).

## Historical WP0 execution model

### Top-level schedule at the historical audit

At the audited WP0 commit, all runtime game systems were registered in `Update`.
The eight legacy `GameSet` values had this hard ordering edge:

```text
Global
  -> Input
  -> Action
  -> Movement
  -> Combat
  -> Items
  -> Respawn
  -> Presentation
```

That set chain was deterministic only at the stage level. It did not make ECS query
iteration deterministic, and systems added separately inside one set are not
ordered unless an explicit edge is listed below.

The current runtime instead uses the TickStart through TickEnd `FixedUpdate` chain
listed in `src/lib.rs` and audited in `determinism-system-audit.md`. The historical
tables below explain preserved behavior and accepted cutover changes only.

### Native startup order

The Steam/native startup systems form one chain in this exact order:

1. `effects::setup_effect_assets`
2. `combat::setup_combat_visual_assets`
3. `bee_skills::setup_bee_skill_assets`
4. `chick_skills::setup_chick_skill_assets`
5. `penguin_skills::setup_penguin_skill_assets`
6. `combat_sfx::setup_combat_sfx_assets`
7. `characters::setup_character_move_catalog`
8. `feel::setup_combat_feel_tuning`
9. `bot::setup_bot_action_control`
10. `map_editor::setup_map_editor`
11. `specials::setup_special_assets`
12. `arena::setup_arena`
13. `items::setup_items`
14. `camera::setup_camera`
15. `fighter::spawn_fighters`
16. `hud::setup_hud`
17. `map_editor::setup_map_editor_ui`
18. `user_mode::setup_user_mode_ui`

This order currently couples combat creation to meshes, materials, scenes, audio,
camera, and UI. A headless authority cannot reuse it as-is. The browser has a
different startup/spawn path; that difference is outside the initial Steam-online
scope and must not be copied into `afc_sim`.

### Deferred-command semantics

`src/lib.rs` does not register explicit `apply_deferred` systems. Bevy 0.18's
default automatic deferred pass inserts a synchronization point on an ordering edge
when the upstream system has deferred state such as `Commands`.

In the tables below, `[D]` means the system has a `Commands` parameter. In a
chained list, commands produced by a `[D]` system are applied before its ordered
successor. Pending commands are also applied before entering the next ordered
`GameSet`. Direct resource and component mutations are immediate; they do not wait
for a command flush.

This produces observable legacy behavior:

- a special spawned by `handle_special_inputs` is visible to later systems in the
  same frame;
- attack and item hitboxes are flushed before `resolve_hitboxes`, so they may hit on
  their spawn frame;
- an expired hitbox resolves in `Action` before its lifetime is decremented and it
  is despawned in `Combat`;
- a penguin surface spawned by the penguin-skill updater is visible to the surface
  updater later in that frame;
- a cannon bomb spawned inside `update_powder_keg_cannons` is not visible to that
  system's already-created query iteration and begins updating on the next frame;
- a command spawned within a system is never visible to another query in that same
  system invocation.

The target simulation must replace these implicit flush effects with explicit
phase-owned spawn/despawn queues. A parity fixture must cover every same-tick spawn
behavior listed above.

### Global stage

The primary native `Global` chain is:

| Order | System | Deferred |
| ---: | --- | :---: |
| 1 | `map_editor::toggle_map_editor` |  |
| 2 | `map_editor::map_editor_input` | `[D]` |
| 3 | `characters::reload_character_move_catalog` |  |
| 4 | `feel::reload_combat_feel_tuning` |  |
| 5 | `user_mode::handle_user_mode_input` | `[D]` |
| 6 | `user_mode::sync_user_mode_controllers` | `[D]` |
| 7 | `game_state::handle_global_input` | `[D]` |
| 8 | `user_mode::sync_user_mode_battle_bot` |  |
| 9 | `user_mode::sync_user_mode_battle_result` |  |
| 10 | `user_mode::sync_user_mode_battle_music` | `[D]` |
| 11 | `user_mode::sync_dev_mode_music` | `[D]` |
| 12 | `user_mode::sync_user_mode_preview_scene` | `[D]` |
| 13 | `game_state::sync_setup_character_scene_models` | `[D]` |
| 14 | `game_state::tick_hitstop` |  |
| 15 | `game_state::tick_match_timer` |  |
| 16 | `game_state::tick_announcements` |  |

`fighter::update_drunk_status [D]` is registered separately in `Global`. It is
explicitly after `tick_hitstop` and `handle_global_input`, but has no ordering edge
against `tick_match_timer` or `tick_announcements`. Bevy may serialize conflicting
access, but the relative order is not a gameplay contract.

The browser-only gameplay-spawn chain is also registered separately in `Global`.
There is no corresponding native runtime branch because native entities are
created in `Startup`.

Hot-reloading character moves and combat feel during an active native process is a
development feature. Online sessions must freeze a validated content manifest
before countdown and disable these reload paths for the match.

### Input stage

The `Input` systems are one chain:

1. `fighter::collect_player_input`
2. `bot::bot_action_control_input` on native builds
3. `bot::bot_input`
4. `fighter::apply_drunk_input_modifier`

No system in this chain uses `Commands`. Human input is reconstructed from raw
device state every render frame. Dash and guard tracking use hidden `Local` state,
and human gesture timing reads `Time::elapsed_secs()`. There is no tick-addressed
input frame or transition accumulator yet.

### Action stage

The `Action` systems are one chain:

| Order | System | Deferred |
| ---: | --- | :---: |
| 1 | `fighter::apply_aim_assist` |  |
| 2 | `items::handle_item_inputs` | `[D]` |
| 3 | `specials::handle_special_inputs` | `[D]` |
| 4 | `specials::tick_special_cooldowns` |  |
| 5 | `equipment::tick_equipment_cooldowns` |  |
| 6 | `fighter::update_fighter_state` | `[D]` |
| 7 | `fighter::update_grab_holds` | `[D]` |
| 8 | `fighter::update_ultimate_locks` |  |
| 9 | `combat::spawn_attack_hitboxes` | `[D]` |
| 10 | `items::spawn_item_hitboxes` | `[D]` |
| 11 | `combat::resolve_hitboxes` | `[D]` |

This is a consequential legacy order. Item/special commands consume input before
normal fighter action interpretation. Hitboxes resolve before fighter movement for
the frame. Impact application mutates a target immediately while nested hitbox and
fighter queries are still iterating.

### Movement stage

The `Movement` systems are one chain:

| Order | System | Deferred |
| ---: | --- | :---: |
| 1 | `fighter::apply_fighter_movement` | `[D]` |
| 2 | `arena::update_arena_pipe_transits` |  |
| 3 | `fighter::separate_fighters` |  |

Fighter movement and arena collision therefore precede pipe movement, and body
separation sees the post-pipe positions. Dust and aftermath effects emitted during
movement are flushed before pipe transit.

### Combat stage

The `Combat` systems are one chain:

| Order | System | Deferred |
| ---: | --- | :---: |
| 1 | `combat::update_hitboxes` | `[D]` |
| 2 | `specials::update_specials` | `[D]` |
| 3 | `bee_skills::update_bee_skills` | `[D]` |
| 4 | `chick_skills::update_chick_skills` | `[D]` |
| 5 | `penguin_skills::update_penguin_skills` | `[D]` |
| 6 | `penguin_skills::update_penguin_surfaces` | `[D]` |
| 7 | `arena::update_arena_hazards` | `[D]` |
| 8 | `arena::update_arena_hazard_visuals` |  |
| 9 | `arena::update_arena_pipe_visuals` |  |
| 10 | `arena::update_crank_yard_machinery` |  |
| 11 | `arena::update_powder_keg_cannons` | `[D]` |
| 12 | `arena::update_vent_spiral_machinery` |  |

Rows 8-12 show a current ownership violation: visual animation and gameplay state
share the same stage, and crank machinery mixes lever/blade presentation with the
gameplay `crank_saws_stopped` toggle.

### Items stage

The `Items` systems are one chain:

| Order | System | Deferred |
| ---: | --- | :---: |
| 1 | `items::drop_items_from_disabled_fighters` |  |
| 2 | `items::update_items` |  |
| 3 | `items::update_moving_items` | `[D]` |

Forced drops happen before loose/respawning item timers, and moving/thrown/armed
items update after both. Mystery-crate rewards and effect entities spawned by the
last system are flushed at the transition to `Respawn`.

### Respawn stage

The stage contains:

- `fighter::ringout_and_respawn [D]`
- `fighter::refill_depleted_practice_health` on native builds

These systems are in one tuple but are not chained. Their relative execution order
is unspecified. Their mutable fighter access prevents useful parallelism but does
not create a semantic ordering edge. Production migration fixtures must not encode
the practice-health helper as an online rule.

Commands from `ringout_and_respawn` are applied before `Presentation` because the
stage chain has a downstream edge.

### Current-to-target phase mismatches

The target pipeline in `multiplayer-architecture.md` is intentionally cleaner than
the current schedule, but adopting it is not mechanically behavior-neutral:

| Current behavior | Target pressure | Required handling |
| --- | --- | --- |
| Item and special commands run before normal fighter action interpretation | One unified per-tick command/action phase | Preserve command priority or approve fixture diffs |
| Attack hitboxes resolve before the frame's fighter movement | Movement precedes collected contact resolution | Compare every attack-boundary fixture; explicitly accept any changed reach/timing |
| Hitbox contact resolves before hitbox age/path/lifetime advances | Dynamic objects normally advance before contact collection | Freeze whether the spawn pose or advanced pose owns each active tick |
| A newly spawned special/skill can update later in its spawn frame | Pool spawn queues may default to next-tick activation | Retain same-tick activation per object kind unless an accepted fixture says otherwise |
| Impacts mutate targets immediately inside nested loops | Contacts must be collected/sorted before mutation | Preserve intentional trades/priorities through explicit arbitration rules |
| Pipe transit and separation run during hitstop after movement returns | A broad frozen movement phase would freeze too much | Keep these as separately gated phases initially |
| Ring-out/respawn advances during hitstop | Cleanup is often grouped with frozen gameplay | Preserve the audited matrix in the initial cutover |

Do not resolve these mismatches by changing the expected output during the same
change that introduces fixed ticking. Capture legacy output first; make any
gameplay-policy change separately and visibly.

### Presentation stage and extra systems

The principal presentation chain is, in order:

1. fighter pose/model synchronization;
2. light-punch corner cues;
3. fighter tint;
4. guard shields;
5. loadout visuals;
6. held-item visuals;
7. visual-effect lifetime updates;
8. arena visual synchronization, then preview render layers;
9. map overlay and native editor preview/gizmos/debug gizmos;
10. feedback-cue lifetimes;
11. native gameplay camera-control hotkeys;
12. camera follow;
13. native editor camera;
14. HUD and native editor UI;
15. user-mode preview rotation, selection previews, UI, and button styles.

Several systems are registered separately in `Presentation`: arena background
camera sync (explicitly after camera follow), dev arena label, HUD plate sync
(explicitly before HUD update), controls UI, UI camera, combat SFX playback, web
battle status, and screen-look transition. Other than the two explicit edges, their
relative order is not guaranteed.

The target must keep all of this work outside the simulation schedule. Presentation
may observe current and previous canonical poses, but must not mutate canonical
state or influence a tick hash.

## Hitstop contract

### Current mechanics

Current hitstop is `Hitstop { remaining_ticks: u32 }`. Authored positive seconds
are converted once with the centralized ceiling-to-ticks rule; a trigger retains
the maximum of the existing and requested tick counts. In the `Match` phase of
each 60 Hz simulation step, `tick_hitstop` performs one saturating decrement.
Downstream systems call `active()` and therefore observe the post-decrement value.

Frozen consequences:

- the render/update and network timelines continue during hitstop;
- the match timer and match phase timer continue during hitstop;
- hitstop is decremented before downstream freeze checks;
- if the decrement reaches zero, the rest of that simulation step is not frozen;
- a new hitstop triggered in `Action` can freeze `Movement` and later systems in the
  same simulation step;
- a new hitstop triggered late in `Combat` can freeze the following `Items` stage;
- a system that ran before the trigger is not retroactively frozen;
- another contact may extend the remaining duration by taking the maximum.

### Current hitstop matrix

The matrix records what happens after `tick_hitstop` while hitstop remains active.
“Runs” means authoritative fields can change. “Frozen” means the system returns or
skips its advancing branch. Presentation always remains frame-rate driven.

| Area | Current behavior during active hitstop |
| --- | --- |
| Match/network timeline | Render updates continue; target `SimTick` and network histories must continue |
| Match timer and phase timer | **Runs**; no hitstop check |
| Hitstop counter | **Runs** first; decremented by exactly one tick with saturation |
| Human device sampling | **Runs**; current `FighterInput` is refreshed |
| Hitstop follow-up/guard-counter buffer | **Runs** inside `update_fighter_state` before its early return |
| Bot AI input generation | **Frozen**; the previous bot input component remains in place |
| Drunk directional modifier | **Runs** on the currently sampled/stale input |
| Drunk duration and bubble cadence | **Frozen** |
| Aim assist | **Runs** and may update facing |
| Item input commands and pickups | **Frozen** |
| Special input commands | **Frozen** |
| Special cooldown | **Frozen** |
| Equipment cooldown | **Runs** |
| Fighter action/status timers | **Frozen**, except the explicit hitstop input buffers above |
| Grab hold/escape/throw progression | **Frozen** |
| Ultimate lock maintenance | **Runs**; lock pairs and locked poses are maintained/released from existing state |
| New attack/item hitbox spawning | **Frozen** |
| Existing hitbox contact resolution | **Runs**; it has no hitstop guard and may hit a not-yet-hit target |
| Fighter movement and movement-owned timers | **Frozen** |
| Arena pipe transit | **Runs** |
| Fighter separation | **Runs** |
| Hitbox lifetime/path motion | **Frozen** |
| Generic specials | **Frozen** |
| Bee/chick/penguin skills and penguin surfaces | **Frozen** |
| Arena hazard clock, cooldowns, and damage | **Frozen** |
| Powder-keg cannon clock, bombs, and damage | **Frozen** |
| Crank-yard lever cooldown/toggle | **Runs**; visual machinery also runs |
| Forced item drops | **Runs** |
| Loose pickup lockout and item respawn timer | **Runs**; Update-only item presentation remains independent |
| Thrown/armed/spraying/rolling item motion and damage | **Frozen** |
| Ring-out detection, stock loss, respawn countdown, and respawn action | **Runs** |
| Native practice refill | **Runs** |
| Announcements, effects, audio, camera, HUD, and visuals | **Runs** at presentation rate |

### Fixed-tick policy

Simulation version 5 preserves this policy:

1. `SimTick` increments exactly once for every 60 Hz simulation step, including
   hitstop steps.
2. Input and network histories advance on every `SimTick`.
3. The match clock and phase clock advance by one tick during hitstop.
4. Canonical hitstop is an integer tick counter. At the beginning of a simulation
   step it is decremented with saturation; phase freeze checks observe the
   post-decrement value. A hitstop triggered later in the step is visible to all
   later phases in that same step.
5. Authored seconds/milliseconds are converted with the centralized duration rule;
   a positive duration never becomes zero ticks. The spawn/trigger step counts as
   one frozen invocation for phases after the trigger.
6. The freeze matrix above remains the initial phase policy. Any cleanup that must
   move to maintain invariants must be covered by an accepted-change fixture.
7. Simulation hitstop never pauses Bevy real time, transport polling, UI, or render
   interpolation. Presentation time scaling is a separate client-only value.

Policy fixtures cover the post-decrement boundary, timer continuation, duration
maximum, input buffering, and rollback presentation deduplication. The broader
single-tape phase-matrix fixture remains useful regression depth and is tracked in
the migration evidence checklist below.

## Implemented authoritative-state inventory

The word “authoritative” here means that a field can affect a future gameplay
outcome or confirmed result in the current implementation. Some listed types also
contain presentation-only fields; the snapshot section separates them.

### Match, manifest, and clocks

| Current owner | Gameplay-relevant contents | Canonical disposition |
| --- | --- | --- |
| `LocalSetup` | selected rules/arena, four slot participants, character/style/equipment/team/input selections, replay seed | Build the immutable match manifest; raw device assignments remain client-only |
| `MatchState` | phase, phase timer, match timer, rules, rule/arena indices, active slots/count, teams, stocks, replay seed, reset state | Canonical match state using stable IDs and integer ticks |
| `MatchTelemetry` | ring-outs, falls, item hits, throws, guard breaks, damage totals, replay seed | Canonical result/stat counters; progression consumes only confirmed values |
| `Hitstop` | canonical `remaining_ticks: u32`; authored seconds convert with ceiling | Include the integer counter directly |
| `MatchAnnouncements` | message string and display timer | Presentation-only; derive from simulation events |
| `ACTIVE_ARENA_INDEX` | process-global `AtomicUsize` | Remove; arena ID is match-owned canonical state |

`debug_hitboxes`, reset hotkeys, local input assignments, and menu selection indices
are not online combat state. The selected stable rule ID, arena ID, loadouts, teams,
and active fighter slots are part of the manifest.

The historical rematch leak is not part of the current contract. Reset/epoch
transitions clear every stable dynamic kind, including Chick skills and cannon
ordnance, rebuild item anchors, clear relationships and counters, and reconstruct
arena runtime from the committed manifest. Native online rematch additionally
retires the old connection generation before revision 2 starts with a new
`MatchId`; stale old-generation traffic cannot mutate the new match.

### Fighter state

Each active fighter is identified by stable slot `FighterId` in `0..4`. The current
authoritative fighter state is spread across ECS components:

| Component/group | Fields that affect gameplay |
| --- | --- |
| `Fighter` | slot ID and spawn point; name/color are presentation metadata |
| Gameplay pose | `SimPosition.translation` is canonical; gameplay facing and size live in motor/status fields. Render-root `Transform` translation is a one-way projected/interpolated copy, and its rotation/scale are presentation-only |
| `FighterCharacter`, `FighterStyle`, `FighterEquipment` | loadout kinds and equipment cooldown |
| `FighterStats` | health, stamina, score, last attacker, invulnerability, respawn/refill state, elemental carry, speed/giant item statuses; `hud_flash` is presentation-only |
| `FighterMotor` | velocity, facing, grounded state, landing/knockdown aftermath, air-use flags, queued-air input, movement timers/limits, ice slide, guard windows/buffers, reaction counters |
| `FighterInput` | current action-level axes/buttons and raw press/release latches consumed by gameplay |
| `FighterActionState` | action, elapsed/charge time, hitbox-spawn flag, combo/technique/button queues, buffered input, confirmed-hit flag, timeline mask, cancel/branch windows, reaction state |
| `DrunkStatus` | remaining duration and gameplay input inversion; bubble cadence/phase is presentation derived from integer tick state |
| `FighterInventory` | held item relationship |
| `FighterGrabState` | holder/victim relationships and regrab lockout |
| `FighterSpecialState` | cooldown |
| `FighterUltimateState` | attacker/target lock relationships |

`Controller`, `PlayerKeyBindings`, raw keyboard/gamepad state, fighter names/colors,
scene hierarchy, visibility, materials, animation pose roots, and HUD markers are
not canonical simulation state.

`BotBrain` is an authority-side input producer, not client-predicted combat state.
Its decision timers, movement plan, named RNG state, and last emitted input must be
captured in an authority checkpoint or reproduced by the stored accepted bot-input
stream. Clients do not run bot AI during prediction.

### Arena and collision state

| Current owner | Gameplay-relevant contents | Canonical disposition |
| --- | --- | --- |
| `ArenaPipeState` | per-fighter candidate endpoint, dwell/cooldown, transit endpoints, elapsed time, entry pose | Canonical, integer-tick, indexed by `FighterId` |
| `ArenaHazardState` | arena index, hazard clock, per-hazard/per-fighter cooldowns, crank stopped flag, lever cooldown | Canonical; bounded arrays in arena-definition order |
| `PowderKegCannonState` | fire timer and alternating-cannon index | Canonical |
| `ArenaCannonBomb` plus `SimPosition` | canonical position, velocity, lifetime | Canonical ordnance pool; render transform is optional client projection |
| `ArenaCollisionWorld` and arena definitions | static supports, barriers, bounds, pipe/hazard definitions and spawn anchors | Static content addressed by arena ID and gameplay-content hash; rebuild/validate on restore |
| `ArenaFighterBurn` | burn visual lifetime/flicker only | Presentation-only; damage/cooldown is in hazard state |

Hazard markers, flames, pipe particles, saw visuals, lever mesh rotations, warnings,
vent meshes, wallpaper, lights, render layers, and arena asset caches are excluded.
The crank **logical** stopped flag remains canonical even though its current updater
also animates those excluded objects.

### Dynamic combat objects

Every live dynamic object below uses a stable `SimEntityId`, bounded pool slot,
generation, and deterministic pool-iteration order.

| Object | Canonical state to retain |
| --- | --- |
| `Hitbox` | owner, source/payload/shape/reaction/damage IDs, attack attributes, lifetime/elapsed, origin/facing/range/radius/path progress, landing flags, hit history |
| `ArenaItem` / `ItemState` | item kind, state, holder/owner, owner fighter ID, timers/grace, durability, pickup lockout, anchor/resting position, gameplay position/velocity, hit history. The loose-item bob phase is presentation-only |
| `ActiveSpecial` | kind, owner, style/payload/shape/source IDs, pose/velocity, lifetime/age/windows/repeat state, radius/grace, stamina effects, hit history |
| `ActiveBeeSkill` | kind, owner, target, payload/shape/source IDs, pose/velocity, timers/repeat state, radius, stamina effects, hit history, size scale |
| `ActiveChickSkill` | kind, owner, payload/shape/source IDs, pose/velocity, timers/repeat state, radius, stamina effects, hit history, size scale, orbit relationships derived or stored explicitly |
| `ActivePenguinSkill` | kind, owner, target, payload/shape/source IDs, pose/velocity, timers/repeat state, radius, stamina effects, hit history, size scale |
| `ActivePenguinSurface` | kind, owner, pose/facing, lifetime/age/next tick, radius, touched-fighter history, size scale |
| Powder-keg ordnance | position, velocity, lifetime and stable ID |

Mesh/material handles, `SceneRoot`, visibility, static string cues, feedback package
IDs used only to choose visuals, and child scene entities are not part of these
objects' canonical representation. A gameplay definition ID replaces static slices
such as a hitbox path; the content hash validates the referenced data.

The item namespace now uses a fixed production pool and a fail-closed crate
lifecycle. Each reward stores its source crate's generational `SimEntityId`; a
crate cannot create a second live reward while that relationship exists. A reward
releases its slot instead of entering the anchor respawn loop, and match/setup
resets sort by stable ID, release every reward/excess item, and rebuild exactly the
selected arena's authored anchors. Other dynamic classes use the same
fixed-capacity allocator and fail-closed overflow contract; release performance
evidence still has to exercise production high-water marks.

### Historical machine-local relationships and implemented replacement

No canonical snapshot type contains Bevy `Entity`. The migration replaced these
historical machine-local relationships:

- `FighterInventory.held`;
- `FighterGrabState.holding` and `held_by`;
- `FighterUltimateState.target` and `owner`;
- `Hitbox.owner` and every element of `Hitbox.already_hit`;
- item held/thrown/armed/spraying owners and `ArenaItem.already_hit`;
- generic-special owner and hit history;
- bee-skill owner, target, and hit history;
- chick-skill owner, dynamic orbit/return references, snapshots, and hit history;
- penguin-skill owner, target, hit history, surface owner/touched history, and
  snowflake-swap references;
- gameplay maps or temporary arbitration records keyed by `Entity`.

Presentation-only `Entity` references such as effect-follow targets, scene children,
and hitbox scene-visual owners remain outside the simulation boundary rather than
being translated into simulation IDs.

Use `FighterId` for fighter-only relationships. Use a generational `SimEntityId`
for dynamic-to-dynamic or polymorphic relationships. A stale generation fails
closed and emits a diagnostic; it must never bind to a newly reused pool slot.
Per-fighter hit/touch histories are fixed bitsets rather than growable
`Vec<Entity>` values.

## Determinism boundaries and historical blockers

### Historical ordering gaps and implemented rules

| Historical gap | Observable risk | Implemented deterministic rule |
| --- | --- | --- |
| Fighter queries are consumed in ECS order | aim ties, item pickup, grabs, impacts, bots, separation and respawns can differ | materialize and sort by `FighterId` |
| Hitbox/special/skill/item/bomb queries are in ECS order | first impact can change whether later contacts are valid | iterate by `(object kind, SimEntityId)` and collect contacts before mutation |
| `resolve_hitboxes` mutates targets in nested query order | trades and simultaneous hits are order-dependent | build contact records, sort by a fixture-owned contact key, then resolve explicit trade rules |
| `nearest_portable_item` and nearest-target selections compare distance without a stable tie-break | equal-distance winner depends on query order | compare `(quantized distance, stable ID)` |
| Fighter separation builds pair corrections from query order | accumulated floating corrections can differ | process canonical `(min FighterId, max FighterId)` pairs and apply sorted reductions |
| Contested pickup iterates fighters and immediately claims an item | pickup winner is machine-local | collect claims and choose the documented priority/tie key |
| Grab and ultimate lock temporary maps use first-seen entries | winner/release can differ | stable source/target keys plus an explicit conflict policy |
| Penguin ice-cap cleanup groups by `HashMap<Entity, ...>` and equal-age order is unspecified | despawn/reuse order can differ | fixed owner arrays; sort `(age, SimEntityId)` with a total tie-break |
| Dynamic ECS spawn/despawn determines future `Entity` order | later contacts and ownership diverge | bounded generational pools with deterministic allocate/free policy |
| Respawn and practice refill are unchained | relative mutation order is undefined | exclude practice helper online; give production respawn one explicit phase |
| Separately registered `Global`/presentation systems have partial orders | a refactor may accidentally rely on executor topology | simulation step owns only explicit phases; presentation cannot feed back |

The contact key and conflict priorities are now gameplay rules implemented by the
bounded contact resolver, stable pool traversal, and fighter-indexed claim slots.
Focused reverse-allocation fixtures lock the source-family rules. Versioned
behavior tapes classify intentionally stabilized legacy ambiguity, such as the
lower-`FighterId` contested item winner, as **undefined legacy order** rather than
claiming parity with an incidental ECS order.

### Implemented time and numeric boundary

- Canonical gameplay advances once per 60 Hz `FixedUpdate` step and does not
  consume render delta or wall-clock time.
- Match, action, reaction, movement, status, cooldown, hitbox, item, skill, hazard,
  pipe, cannon, respawn, and hitstop durations use integer tick types.
- Device transitions are accumulated and compiled into per-tick action frames;
  bot and gameplay randomness use master-seed-derived named deterministic streams.
- Authoritative vector length, distance, and normalization use explicit scalar
  order and the pinned software `libm` path. Runtime authoritative trigonometric
  choices were replaced by frozen lookup data where required.
- Continuous canonical values remain `f32` behind a defined math boundary and are
  quantized to the 1/4096 grid at TickEnd before serialization and hashing.
- Stable content IDs and the frozen gameplay-source digest prevent runtime
  character/feel hot reload from changing an online match.

Cosmetic sine waves remain legal in presentation. Fixed-point escalation remains a
release contingency if the supported-platform deterministic tape diverges;
physical Steam Deck verification is still external.

### Implemented presentation boundary

Canonical fighter, hitbox, special, character-skill, surface, and cannon
translations live in `SimPosition`; items retain their canonical
`ArenaItem::position`. Render `Transform`, mesh/material/scene handles, visibility,
animation, camera, audio, HUD, and effect lifetimes are excluded from snapshots and
hashes. Update-only projection attaches and synchronizes render components from
canonical pose.

Gameplay emits ordered `SimEvent` facts with deterministic IDs plus bounded typed
presentation-intent journals. Client consumers deduplicate irreversible effects,
discard future predicted intents on rollback, and route progression from confirmed
authority results only. The production headless world omits windows, render/audio
assets, cameras, UI, and presentation consumers. Optional cosmetic sidecars still
co-located in a few fixed systems cannot feed canonical state and are absent when
their client-only resources are not installed.

## Canonical snapshot inventory

A full snapshot must restore the simulation at the end of a tick without consulting
older mutable gameplay state. Fields are serialized in the exact order below, not
by Rust memory layout or ECS archetype order.

### Include

1. **Header**
   - snapshot schema version;
   - simulation/protocol compatibility version;
   - gameplay-content hash;
   - match ID or manifest digest;
   - snapshot `SimTick` and master match seed.
2. **Match**
   - phase and phase ticks;
   - match ticks remaining;
   - stable rules and arena IDs;
   - active slots, teams, stocks, and canonical result state;
   - hitstop ticks;
   - any deterministic event ordinal needed to continue the current tick boundary.
3. **Input state, in `FighterId` order**
   - last accepted quantized axes and held-button set;
   - edge/substitution latches needed by the next step;
   - action buffers already transferred into fighter state.
4. **Fighters, fixed array `[0; 4]`**
   - occupancy/active flags and `FighterId`;
   - canonical gameplay pose, velocity, facing, grounded/collision state;
   - character/style/equipment IDs and gameplay cooldowns;
   - health, stamina, score, stock-related attribution and status values;
   - motor flags/timers, action timeline and reaction state;
   - inventory, grab, ultimate, and special relationships using stable IDs;
   - gameplay statuses such as elemental carry, giant/speed, and drunk duration.
5. **Arena runtime**
   - pipe states for four fighters;
   - hazard clock, logical devices, and fixed per-target cooldown arrays;
   - cannon state and any other dynamic device counters.
6. **Dynamic pools, by kind then pool index**
   - pool capacity, occupied bitset, generation per slot, free-list/cursor state;
   - full canonical item, hitbox, generic-special, bee-skill, chick-skill,
     penguin-skill, penguin-surface, and ordnance records;
   - dynamic collision state and stable relationship/hit bitsets.
7. **Randomness**
   - master seed;
   - each named gameplay RNG stream's state and consumption counter.
8. **Canonical statistics**
   - match telemetry and every result/progression-relevant counter.

The pool allocator state is mandatory. Restoring object values without allocator
generations/free order can assign different future IDs and diverge later.

### Exclude

- all Bevy `Entity` values and raw ECS allocation state;
- raw `Transform`/`GlobalTransform`; snapshot only extracted canonical gameplay
  pose fields, never visual child transforms;
- `Handle`, `SceneRoot`, mesh, material, font, audio, image, render layer, and asset
  cache values;
- visibility, animation bones/pose roots, tint, guard shields, markers, trails,
  particles, effect-follow state, and hitbox scene visuals;
- `HitEffects`, camera shake/follow/control state, screen-look transitions, rumble,
  and audio queues;
- announcement/HUD/debug strings, `hud_flash`, editor state, debug-hitbox flags,
  preview readiness, and user-mode UI state;
- fighter labels/colors and static string pointers;
- raw keyboard, mouse, gamepad, Steam Input, or local controller assignments;
- Steam identities, lobby/socket/ticket objects, RTT, packet queues, and other
  session/transport state;
- static arena geometry, move definitions, feel tuning, character definitions, and
  item definitions already identified and validated by stable ID/content hash;
- client render interpolation/correction history and presentation-event consumed
  IDs; these have their own bounded client state;
- bot AI internals in a client rollback snapshot. Authority crash/recovery
  checkpoints must additionally capture bot input-generator state, or continue from
  a recorded future bot input stream.

### Snapshot audit rule

For every authoritative component/resource added during multiplayer work, the same
change must update one of the Include/Exclude lists and add a restore test. An
excluded field that can change a future tick hash is a defect. An included field
that contains a platform handle or presentation state is also a defect.

## Migration acceptance fixtures

### Fixture format and harness

Create version-controlled fixture inputs rather than test functions full of ad-hoc
key presses. A fixture contains:

```text
Fixture
  name and contract version
  match manifest (rules, arena, slots, teams, loadouts, seed, content hash)
  deterministic initial-state overrides
  per-step InputFrame[4]
  optional deterministic dynamic-object injections
  checkpoint ticks
  expected canonical fields, ordered SimEvents, and final result
  classification: Preserve | AcceptedChange | UndefinedLegacyOrder
```

Before WP1 changes gameplay scheduling, capture a **legacy 60 Hz** result using an
exact scripted delta, fixed spawn order, frozen content, and no live devices. Store
semantic assertions plus a normalized canonical-state dump; omit every snapshot
exclusion above. Once canonical hashing exists, append the hash for every tick.

The versioned implementation lives in `tests/fixtures/behavior/v1`, with its
crate-internal runner at `tests/support/behavior_fixtures.rs`. It always constructs
the real manifest/snapshot contract through `build_headless_match_config`, builds
the real render-free world through `build_headless_simulation`, and advances only
through `LiveSimulationDriver::step_committed`. Raw and action-level scripts are
compiled into one immutable input tape before any world or restore exists because
local gesture history is intentionally not snapshot state.

Each fixture records the canonical hash for every completed tick plus bounded
normalized checkpoints, ordered semantic events, and the final result. The runner
requires two clean executions in one process, a same-world snapshot restore/replay,
and a fresh world containing an unrelated presentation-only entity to produce the
same trace. No canonical state is directly mutated after tick zero.

The normal test command reads goldens and never rewrites them. The only writer is
an ignored, environment-gated updater:

```bash
AFC_UPDATE_BEHAVIOR_GOLDENS=1 cargo test --locked --lib \
  headless::behavior_fixtures::update_behavior_fixture_goldens \
  -- --ignored --exact --nocapture
```

The updater refuses to run without the exact opt-in value and reports the first
changed hash plus checkpoint/event/final-result counts before replacing a file.
During fixture authoring, `AFC_BEHAVIOR_FIXTURE=<exact fixture name>` may narrow
that same gated command to one checked-in tape; an unknown name fails without
writing. Release evidence always runs the unfiltered command followed by the
ordinary read-only suite.
An intentional behavior change still requires a simulation-version entry in the
change log; running the updater is evidence generation, not approval.

The gameplay-content digest is deliberately conservative: a source file can
contain both authoritative and presentation-only code and still appear in
`GAMEPLAY_SOURCES`. When such a source-only edit changes the digest but not the
simulation contract, a golden refresh is permitted without changing the
simulation version only if all of the following are recorded:

1. the exact previous and current content digests and the non-gameplay reason;
2. all checked-in tapes receive hashes derived from the current identity;
3. checkpoint counts, ordered semantic-event ticks, final ticks, and final results
   are unchanged; and
4. the ordinary read-only corpus passes after the explicit updater.

This is still an online compatibility change: peers with the old content digest
must not enter the new match. If any checkpoint field, event payload/order, final
tick, or result changes, the edit is not content-identity-only and must use the
normal accepted-change and simulation-version process.

### Required preservation fixtures

| Fixture | Minimal input/setup | Required observations |
| --- | --- | --- |
| `move_ground_accel_stop` | one fighter holds a cardinal direction, then releases | per-step position/velocity/facing, grounded state, stop timing |
| `move_air_control_land` | directional input spanning takeoff and landing | air control, support collision, landing state/timers |
| `jump_tap` | one jump edge | takeoff tick, apex checkpoint, landing tick/action |
| `dash_gesture_and_motion` | scripted fast double tap and action-level dash | input gesture is not lost; dash cost, velocity, duration, recovery |
| `light_combo` | light edges at early, valid branch, and late timings | accepted/rejected queue windows, hitbox spawn steps, final action |
| `heavy_charge_release` | hold and release heavy around charge boundaries | charge ticks, selected payload, stamina, startup/active/recovery |
| `guard_hit` | attack into held guard | stamina damage, knockback/reaction, guard hitstop |
| `perfect_guard_counter` | guard/counter at both sides of the timing boundary | boundary inclusion, buffered counter, resulting action/contact |
| `grab_hold_escape` | grab followed by victim escape input | holder/victim links, hold timeout, release and lockout |
| `grab_throw_directions` | quick/heavy/timeout throws with directional input | throw direction, damage/knockback, attribution, relationship cleanup |
| `item_pickup_use_throw` | pick up each item role, use or throw it | ownership, durability, action consumption, impact, respawn state |
| `mystery_crate_rng_cutover` | open crate at fixed pose/time for two seeds | current accepted rule: the same seed repeats and a selected different seed changes the named-stream reward; the old wall-time result is historical provenance only |
| `bot_rng_cutover` | run one bot against a fixed opponent/input tape for two seeds | current accepted rule: bot input is seed/tick driven and reproducible; a selected different seed changes it |
| `generic_special_variants` | cast projectile/trap/shockwave/hazard | cooldown, spawn step, active contacts, expiry/despawn |
| `character_skill_lifecycle` | exercise each bee/chick/penguin dynamic kind | stable spawn/update/contact/child spawn/despawn lifecycle |
| `arena_hazard_contact` | one fighter crosses each hazard boundary | active-window boundary, cooldown, damage/reaction/attribution |
| `arena_pipe_transit` | dwell at endpoint and complete transit | dwell threshold, pose/action during transit, exit/cooldown |
| `powder_cannon_bomb` | fixed arena/cannon sequence | spawn step, first motion step, contact/ground detonation, next cannon |
| `ringout_respawn` | cross ring-out bound with stock remaining | stock/score attribution, hidden/respawn timing, pose and invulnerability |
| `knockout_deferred_until_land` | health reaches zero during airborne aftermath | knockout is delayed through required reaction, then resolved once |
| `last_stock_match_completion` | lose final required stock | winner/result, phase transition, confirmed counters exactly once |
| `timed_match_completion` | clock reaches zero | exact time-up and results steps; match clock advances through hitstop |
| `rematch_full_cleanup` | rematch on the same arena with live chick skills, pipe transit, hazards, and cannon bombs | current accepted rule: all pools, links, arena clocks, allocators, and connection-generation state reset or retire exactly once |

The current 17-tape v5 corpus is material but not exhaustive. BF004 is the full
named dash preservation tape; BF026 covers escape, regrab lockout, timeout, and
cleanup; BF027 covers same-tick quick and directional-heavy throws; and BF023
covers the hitstop decrement/restore boundary. BF015 is representative vent
hazard coverage, not every hazard, and BF028 is representative apple-use and
turkey-throw coverage, not every item role. BF024/BF025 lock two previously
undefined contested outcomes. The broader preservation gate remains unchecked
until the remaining rows and catalog matrices are captured.

Character, style, equipment, arena, item, move, special, and hazard coverage may be
generated as a matrix around these core fixtures, but every generated case must have
a stable name and appear in test output.

### Required hitstop fixtures

| Fixture | Required result |
| --- | --- |
| `hitstop_decrement_boundary` | post-decrement zero permits the step; positive remainder applies the freeze matrix |
| `hitstop_trigger_mid_step` | hit in Action freezes Movement and later guarded phases in that same step |
| `hitstop_match_clock_runs` | `SimTick`, input history, match timer, and phase timer advance once per hitstop step |
| `hitstop_counter_matrix` | equipment cooldown, pipe, separation, loose/respawn item timers, and respawn advance; special cooldown, movement, hazards, and moving items do not |
| `hitstop_existing_hitbox_contact` | an existing hitbox may resolve against a new valid target while lifetime/path motion remains frozen |
| `hitstop_duration_max` | overlapping triggers retain the maximum duration and do not add durations |
| `hitstop_presentation_not_replayed` | rollback over the impact does not duplicate audio/effect/camera event IDs |

### Required contested and simultaneous fixtures

These fixtures must be run with at least two unrelated presentation spawn orders and
two dynamic pool-allocation permutations.

| Fixture | Decision to freeze |
| --- | --- |
| `trade_two_strikes` | whether both pre-existing valid contacts land and the exact trade reaction order |
| `two_hits_one_target` | explicit contact priority when first impact can invalidate the second |
| `grab_vs_grab` | stable winner or intentional mutual outcome |
| `two_grabbers_one_victim` | claim priority and cleanup of the loser relationship |
| `two_fighters_one_item` | pickup claim key, including exact-distance tie |
| `ultimate_lock_conflict` | one attacker/target relationship per fighter and deterministic rejected lock |
| `hazard_and_strike_same_step` | ordering/attribution between arena and fighter impacts |
| `throw_and_projectile_same_step` | ordering, hitstop maximum, and last-attacker attribution |
| `simultaneous_ringouts` | stocks, scores, winner/draw semantics independent of fighter query order |
| `final_stock_trade` | match result policy when final eligible fighters lose on one tick |
| `respawn_space_conflict` | deterministic separation/placement for simultaneous respawns |
| `pool_capacity_overflow` | deterministic drop/reject policy with no allocation growth |

If any current result changes when only ECS entity allocation changes, record the
legacy fixture as `UndefinedLegacyOrder`. The target expected result must then name
its stable arbitration key before WP2 can pass.

### Fixture checkpoints

Every fixture records, as applicable:

- tick, phase, clock, hitstop, stocks, score, and result;
- each fighter's quantized pose, velocity, facing, stats, action, reaction, timers,
  status, loadout, and stable relationships;
- dynamic pool occupancy, generations, canonical object records, and overflow;
- arena hazard/pipe/device state;
- named RNG states and consumption counts;
- ordered `SimEventId` plus event payload;
- canonical tick hash; and
- confirmed result/progression invocation count.

Assertions must use exact integer/quantized values. A legacy-only floating capture
may use a field-specific tolerance for diagnosis, but the target canonical fixture
must not use approximate hash equality.

## WP migration gates derived from this contract

These checkboxes distinguish implementation from retained migration evidence.
Unchecked legacy fixture breadth or measured baselines remain evidence debt; they
do not imply that the fixed-tick runtime still uses the historical architecture.

### Historical WP1 cutover evidence

- [ ] Capture all preservation, hitstop, and contested legacy fixtures at scripted
  60 Hz.
- [ ] Record `FourBotStress`, `MapCycle100`, and `Soak10Minutes` baselines under
  [performance.md](performance.md).
- [x] Confirm the historical schedule and `[D]` boundaries against the audited
  source.
- [x] Freeze the integer-tick hitstop policy and timer behavior in tests.
- [x] Disable or isolate runtime content hot reload for a frozen match manifest.
  The production headless authority and prediction composition now parses validated
  embedded character/feel authorship. Native loose-file defaults and file watchers
  remain confined to the rendered developer sandbox.

### WP2 determinism implementation

- [x] No canonical field contains `Entity`, frame time, wall time, raw device state,
  asset handles, or process-global arena state.
- [x] Every query-dependent gameplay path listed above uses stable ordering.
- [ ] Every contested fixture has a named arbitration key and exact expected result.
- [x] All gameplay randomness consumes a named seed-derived stream.
- [x] Pool capacities, generation reuse, and overflow results are fixture-covered.

### WP3 snapshot implementation

- [x] Every Include group round-trips through canonical serialization.
- [x] Every Exclude group can be changed or removed without changing a tick hash.
- [x] Every one of the 17 checked-in behavior tapes restores at its declared
  `restore_tick` and replays to the same remaining per-tick hashes and final
  result.
- [ ] Restore at each high-risk fixture checkpoint and replay to the same per-tick
  hashes and final result across the complete required fixture/catalog matrix.
- [x] Allocation generations/free state and RNG stream counters round-trip.

### WP4 presentation-boundary implementation

- [x] Simulation runs with render, audio, UI, camera, effects, and asset resources
  absent.
- [x] Every current direct cue/spawn is represented by an ordered simulation event
  or is explicitly cosmetic client-only work.
- [x] Re-simulation deduplicates effects and invokes confirmed result/progression
  hooks exactly once.
- [x] Gameplay hashes are identical across presentation quality levels and unrelated
  presentation entity order.

All code changes remain subject to repository validation:

```bash
cargo run
cargo test
```

Measured hot-path changes also require same-hardware before/after evidence under
[performance.md](performance.md).

## Contract change log

| Date | Simulation version | Fixture(s) | Change and approval |
| --- | --- | --- | --- |
| 2026-07-23 | Pre-versioned legacy | Initial inventory | Audited current behavior; no behavior change |
| 2026-07-23 | 4 | `canonical_vector_math_has_explicit_adversarial_semantics`, `canonical_vector_math_q12_corpus_matches_frozen_digest`, `cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`, v3/v4 handshake and replay compatibility fixtures | **AcceptedChange:** authoritative vector length/distance/normalization now uses scalar-ordered operations and the software path in exact `libm 0.2.16`; threshold-only geometry uses squared comparisons with preserved radii; the Chick ultimate uses a frozen 16-way basis. The first production-tape hash divergence is tick 1, as expected from the v4 snapshot identity and numeric contract. Final tick 709 and team-1 result are unchanged. Approved by the multiplayer architecture implementation scope. |
| 2026-07-23 | 5 | BF008 `aim_grab_short_tap`, the complete behavior-fixture corpus, `cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`, `v4_client_is_rejected_by_the_v5_lobby_handshake`, `v4_replay_is_rejected_by_v5_playback_compatibility` | **AcceptedChange:** raw `AIM_GRAB` now compiles as aim while held and exactly one grab on release through the inclusive five-tick grace; holding through the boundary cancels it, accumulated taps survive zero-tick render frames, and guard/ultimate priority remains exclusive. The stock tape's current hashes change at tick 1 only because simulation version is canonical; rewriting only the discriminator reproduces every historical v4 checkpoint and final hash, while final tick 709 and team-1 result remain unchanged. Lobby and replay compatibility reject v4 before gameplay. Approved by the multiplayer architecture implementation scope. |
| 2026-07-24 | 5 (unchanged) | BF004 `dash_gesture_and_motion`, BF015 `arena_hazard_contact`, BF023 `hitstop_decrement_boundary`, BF026 `grab_escape_lockout_timeout`, BF027 `quick_directional_heavy_throw`, BF028 `item_use_throw_impact_respawn`, plus the focused production hitstop/RNG tests indexed in [behavior-fixture-index.md](behavior-fixture-index.md) | **Preservation evidence:** added exact v5 evidence for dash, a representative vent hazard, hitstop decrement/restore, grab escape/lockout/timeout, quick and directional-heavy throws, and representative item use/throw/impact/respawn. The production RNG and direct hitstop tests close the focused rules named in the index. This records existing v5 behavior; it accepts no gameplay change and does not bump the simulation version. Catalog-wide hazard/item and full hitstop-counter coverage remain open. |
| 2026-07-24 | 5 (unchanged) | All 17 behavior tapes; powder-cannon bomb render-parent visibility | **ContentIdentityOnly:** the bomb's logical render parent now receives inherited visibility before its mesh child is attached. The presentation fix is co-located in `arena.rs`, which is conservatively included in `GAMEPLAY_SOURCES`, so the gameplay-content digest changed from `940ffd1093dd6b02df5413b80aa8b8447e0987821fe585c0297ae0c514a8b629` to `b0962f667795d7f2d530bdf3b7606b4a361f9f81c11172f6c3c021983efd9d9c`. All 17 tapes received new per-tick hashes, while checkpoint counts, semantic-event ticks, final ticks, and final results remained unchanged. This changes content compatibility only; simulation version 5 remains unchanged. |
