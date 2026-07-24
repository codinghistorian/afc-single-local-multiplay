# FixedUpdate determinism system audit

Status: implementation audit, 2026-07-24
Source of truth: the production `FixedUpdate` registrations in `src/lib.rs`

## Purpose and acceptance rule

This document is the work checklist for deterministic online simulation. It audits
every production system registered in `FixedUpdate` for three failure modes:

1. a gameplay countdown advanced as `f32` or from frame/wall-clock time;
2. a Bevy `Entity` retained in canonical component or resource state; and
3. gameplay output that depends on raw ECS query or deferred-command order.

`PASS` means the current canonical result is independent of local Bevy entity
allocation. `FIXED` means this audit changed and covered the rule with a fixture.
`PASS (mixed P)` means canonical behavior passes, but presentation work still runs
inside the fixed system and must be extracted for a clean headless authority.
`ARCH` identifies an audited architectural follow-up that is deterministic today
but not yet at its intended multiplayer boundary. Contact arbitration has no
remaining `ARCH` rows: every damage/status producer uses the shared bounded batch.

The identity column uses these terms:

- `stable`: canonical relationships use `FighterId` or generational `SimEntityId`;
- `boundary`: local `Entity` is used only to access the current ECS world or by the
  `SimulationIdentityAllocator` mapping, and is never serialized;
- `P-only`: the `Entity` belongs to a presentation-only component or effect; and
- `none`: the system does not need an entity handle.

## Audit result

- All 52 production fixed systems are listed below.
- No fixed gameplay system reads variable `Time::delta_secs()`, wall time, or
  render time. Gameplay lifetimes and windows use `TickTimer`, `ElapsedTicks`,
  `SimTick`, or integer tick counters.
- Remaining fixed-step `f32` countdowns are presentation-only:
  `HitboxSceneVisual::{elapsed,lifetime}` and `BeeSwarmOrbiter::age`. Authored
  gameplay seconds are converted once at the state boundary with
  ceiling-to-tick policy. Drunk-bubble and dash-trail cadence no longer retain
  mutable presentation clocks; both derive from canonical integer tick state.
- `ArenaItem::Spraying::spiral_radius` is a gameplay float decay rather than a
  duration. It and `spiral_phase` are quantized by
  `ArenaItem::canonicalize_snapshot_floats()` during the shared TickEnd
  canonicalization boundary.
- No canonical gameplay relationship stores Bevy `Entity`. The intentional local
  exceptions are the stable-ID allocator map, boundary-only ECS lookups, and
  presentation-only owners/follow targets.
- Rollback-owned world translation is stored in `SimPosition` (or in the
  canonical `ArenaItem::position` field for items). Fixed gameplay, snapshots,
  restore, and hashing do not read render `Transform`; Update-only projection
  derives render transforms from canonical pose.
- Stable dynamic traversal is by `SimEntityKind` pool slot/`SimEntityId`; fighter
  traversal and tie-breaks are by `FighterId` where ordering can change results.
- Every damaging or status-producing overlap is frozen into one bounded
  `ContactBuffer`. Dynamic sources use generation-checked `SimEntityId`; persistent
  hazards use bounds-checked `(arena index, hazard index)` identity.

Simulation version 4 additionally fixes the authoritative vector-math boundary:

- canonical two- and three-dimensional squared length/distance have explicit
  scalar operation order;
- canonical length/distance/normalization use the software path in the exact
  direct `libm 0.2.16` dependency;
- threshold-only overlap/range checks square their validated authored radius,
  while speed and falloff calculations retain canonical square root semantics;
- the Chick ultimate uses a fingerprinted frozen sixteen-way relative basis
  instead of runtime gameplay trigonometry; and
- local presentation math remains outside the canonical boundary.

The adversarial vector test covers zero, signed zero, subnormal underflow,
non-finite values, overflow, and the minimum Q12 quantum. The frozen Q12 corpus
digest is `74eb67fd4138faa4`.

Simulation version 5 retains that math contract and changes the shared local
gesture compiler. `AIM_GRAB` is aim while held and emits one grab only for a
release inside the inclusive five-tick window (including a complete accumulated
tap); a hold through the boundary cancels it. Guard/ultimate priority and
light/heavy staging are explicit in focused raw-accumulator and action-frame
tests. Exact v4/v5 handshake mismatch fails closed before simulation, while
protocol 1, snapshot schema 2, and replay schema 1 remain unchanged.

Rollback/presentation purity was re-audited at the fighter snapshot boundary:

- `FighterStats::hud_flash` is written/decayed for HUD feedback but is never read
  by a semantic-event predicate, order decision, or payload builder.
- `FighterActionState::reaction_visual_side` feeds render pose only. Ground-bounce
  propagation stays within the excluded pose field; canonical reaction family and
  motion own lifecycle selection.
- `QueuedAftermath::cue` cannot select event kind, order, or ID. Landing emission
  and snapshot decode re-derive it from the complete canonical aftermath tuple;
  unknown or ambiguous tuples fail snapshot restore instead of retaining a
  future-side cue.
- Already-presented IDs remain in `PresentationEventRouter` across rollback, so
  identical irreversible presentation events are suppressed during resimulation.

## Exact system matrix

| Phase | Production system | Clock/countdown audit | Identity audit | Iteration / Commands rule | Verdict |
| --- | --- | --- | --- | --- | --- |
| TickStart | `simulation::advance_sim_tick` | One integer `SimTick` advance | none | No query or commands | PASS |
| TickStart | `sim_event::begin_sim_event_tick` | Uses current `SimTick` | none | Resets one bounded buffer | PASS |
| TickStart | `arena::sync_active_arena_from_match_state` | No timer | none | Single resource copy | PASS |
| TickStart | `ecs_identity::reclaim_orphaned_sim_entities` | No timer | boundary | Scans `SimEntityKind::ALL` and deterministic pool slots; releases exact stable generation | PASS |
| TickStart | `interpolation::begin_sim_pose_tick` | No timer | boundary | Per-fighter field-local update; query order cannot affect another fighter | PASS |
| Match | `game_state::tick_hitstop` | Integer `remaining_ticks` saturating decrement | none | Single resource | PASS |
| Match | `game_state::tick_match_timer` | Integer match/phase ticks | none | Phase event has one ordered presentation side effect | PASS |
| Match | `fighter::update_drunk_status` | Gameplay duration is `TickTimer`; bubble cadence/phase derive from its canonical remaining ticks | boundary | Fighter-local status emits an ordered semantic lifecycle event plus optional presentation sidecar | PASS |
| Input | `fighter::consume_local_player_input` | Tick-addressed `SimTick` drain | none | Per-seat frame is cached once, so duplicate seat reads do not depend on query order | PASS |
| Input | `bot::bot_input` | All brain windows are `TickTimer`; choices are keyed by seed/fighter/tick | stable | Equal target distance breaks by `FighterId`; special/item sources sort by `SimEntityId`; bot mutations are fighter-local | FIXED |
| Input | `fighter::apply_drunk_input_modifier` | No timer mutation | none | Per-fighter input inversion | PASS |
| Action | `fighter::apply_aim_assist` | No timer | stable | Equal distance now breaks by target `FighterId`, independent of query order | FIXED |
| Action | `items::handle_item_inputs` | Item lockouts/lifetimes are tick based | stable | Fighters traverse `FighterId::ALL`; exact-distance pickup breaks by item `SimEntityId`; lower fighter ID wins a contested same-tick claim | PASS |
| Action | `specials::handle_special_inputs` | Cooldown/lifetime converted to ticks | stable | Fighters traverse `FighterId::ALL`; stable pool allocation fails closed on overflow | PASS |
| Action | `specials::tick_special_cooldowns` | `TickTimer::tick` | none | Per-fighter field-local update | PASS |
| Action | `equipment::tick_equipment_cooldowns` | `TickTimer::tick` | none | Per-fighter field-local update | PASS |
| Action | `fighter::update_fighter_state` | Gameplay windows use `TickTimer`/`ElapsedTicks`; dash-trail cadence derives from canonical action elapsed ticks | stable | Authoritative mutations are fighter-local; lifecycle output uses ordered semantic events and optional sidecars | PASS |
| Action | `fighter::update_grab_holds` | Regrab/hold/escape windows are tick based | stable | Relationships are `FighterId`; valid one-holder/one-victim state resolves independently of local entity allocation | PASS |
| Action | `fighter::update_ultimate_locks` | Release window compares `ElapsedTicks` | stable | Relationships and lookup keys are `FighterId`; reversed-entity fixture covers the boundary | PASS |
| Action | `combat::spawn_attack_hitboxes` | Timeline is integer milliseconds derived from `ElapsedTicks`; spawned lifetime is `TickTimer` | stable | Fighter/event traversal is canonical; authoritative spawns use stable pools; scene spawns are presentation only | PASS (mixed P) |
| Action | `items::spawn_item_hitboxes` | Startup compares tick-derived elapsed; hitbox lifetime is `TickTimer` | stable | Fighters traverse `FighterId::ALL`; hitboxes allocate from the stable pool | PASS |
| Movement | `fighter::apply_fighter_movement` | Gameplay timers tick exactly once; integration uses constant `SIM_DT_SECONDS` | stable | Fighter motion is field-local; surface overlap reduction is order-insensitive; dust/cues are presentation commands | PASS (mixed P) |
| Movement | `arena::update_arena_pipe_transits` | Dwell, cooldown, and transit use integer ticks | stable | State is fixed array indexed by fighter ID; exit occupancy is an order-insensitive snapshot predicate | PASS |
| Movement | `fighter::separate_fighters` | No countdown | boundary | Snapshots sort by `FighterId`; pairs are canonical and correction reduction follows that pair order | FIXED |
| Combat | `combat::begin_contact_collection` | No timer; starts one tick-addressed collection epoch | none | Clears logical length/outcomes of one preallocated bounded buffer; cumulative overflow diagnostics are retained | FIXED |
| Combat | `combat::update_hitboxes` | Gameplay lifetime/elapsed are tick types; scene-visual lifetime/elapsed are presentation `f32` | stable plus P-only owner | Gameplay traverses stable hitbox slots; visual query/commands cannot feed canonical state | PASS (mixed P) |
| Combat | `combat::collect_hitbox_contacts` | No variable time; all hit histories/timers are tick based | stable, boundary query | Freezes every eligible fighter-hitbox overlap into the bounded buffer before any target mutation; sources traverse stable pool slots and targets use `FighterId` | FIXED |
| Combat | `specials::collect_special_contacts` | Lifetime, grace, age, and repeats are integer tick/millisecond state | stable | Advances sources in stable pool order and freezes all eligible targets without target/source-outcome mutation | FIXED |
| Combat | `bee_skills::collect_bee_skill_contacts` | Skill lifetime/age/repeat are tick based | stable | Stable source traversal; repeat-window lifecycle precedes geometry; all eligible targets are frozen even for consumed projectiles | FIXED |
| Combat | `chick_skills::collect_chick_skill_contacts` | Skill lifetime/age/repeat are tick based | stable | Stable source traversal and complete frozen target collection; child/source lifecycle is deferred | FIXED |
| Combat | `penguin_skills::collect_penguin_skill_contacts` | Skill lifetime/age/repeat are tick based | stable | Stable source traversal and complete frozen target collection; post-hit warp/trail/landing work is deferred | FIXED |
| Combat | `penguin_skills::update_penguin_surfaces` | Lifetime/age/next emission are tick based | stable | Surfaces are traversal state, not damage contacts; cleanup sorts/deduplicates stable IDs | PASS |
| Items | `items::drop_items_from_disabled_fighters` | Rolling/lockout state is tick based | stable | Fighters traverse `FighterId::ALL`; drop RNG is keyed by seed/tick/item/fighter | PASS |
| Items | `items::update_items` | Respawn/lockout use `TickTimer`; item state age is integer ticks | stable component | Each item's canonical `ArenaItem::position` and timers are independent; Update-only `sync_item_visuals` derives bob/rotation into a render `Transform` | PASS |
| Items | `items::advance_moving_items_and_collect_contacts` | Thrown/armed/spray/rolling lifetimes and cadence are tick based; spray floats are TickEnd-quantized | stable | Stable item traversal freezes every thrown/blast/status target; source durability, transitions, crate children, and spray status are deferred | FIXED |
| Items | `arena::advance_arena_hazards_and_collect_contacts` | Hazard clock and per-fighter cooldowns are tick based | stable authored identity | Authored hazard/fighter traversal only collects; typed arena/hazard indices are bounds-validated centrally | FIXED |
| Items | `arena::update_crank_yard_machinery` | Toggle cooldown is `TickTimer`; animation uses constant fixed dt/tick-derived elapsed | none | Toggle is one order-insensitive `any` predicate; all raw visual queries are field-local | PASS (mixed P) |
| Items | `arena::advance_powder_keg_cannons_and_collect_contacts` | Fire/bomb lifetime are `TickTimer`; motion uses constant dt | stable | Bombs traverse stable ordnance slots and freeze every eligible target before detonation/despawn | FIXED |
| Items | `combat::resolve_contacts` | No variable time; all collected contacts carry tick-derived payload state | typed stable, boundary query | In-place canonical sort; validates dynamic generations/static indices; commits strikes/status outcomes before deterministic cinematic/grab claims | FIXED |
| Items | `combat::apply_hitbox_contact_outcomes` | No timer mutation beyond hitbox state already represented in the outcome | stable, boundary query | Accepted/guarded outcomes update hit memory; authored single-use claim attempts consume exactly once after arbitration | FIXED |
| Items | `items::apply_item_contact_outcomes` | Uses the current `SimTick` handoff only | stable | Stable item order commits hit memory, one durability cost/source transition, crate child, explosion effects, and multi-target barrel status | FIXED |
| Items | `bee_skills::apply_bee_skill_contact_outcomes` | No independent contact timer | stable | Generation revalidation; accepted/guarded outcomes commit hit memory, slow, consumption, child spawn, and lifecycle | FIXED |
| Items | `chick_skills::apply_chick_skill_contact_outcomes` | No independent contact timer | stable | Generation revalidation; accepted/guarded outcomes commit hit memory, consumption, child spawn, and lifecycle | FIXED |
| Items | `penguin_skills::apply_penguin_skill_contact_outcomes` | No independent contact timer | stable | Generation revalidation; accepted/guarded outcomes commit hit memory and authored trails/landing/warp/source lifecycle | FIXED |
| Items | `specials::apply_special_contact_outcomes` | No independent contact timer | stable | Generation revalidation; accepted/guarded outcomes commit hit memory, stamina disruption, consumption, and lifecycle | FIXED |
| Items | `arena::apply_powder_keg_contact_outcomes` | Uses the current `SimTick` handoff only | stable | Stable ordnance detonation set despawns only after every frozen target resolves | FIXED |
| Items | `arena::apply_arena_hazard_contact_outcomes` | Per-target cooldown is assigned only after acceptance | typed authored identity | Accepted/guarded outcomes commit cooldown/snare and attach accent to the resolver's typed event ID | FIXED |
| Respawn | `fighter::ringout_and_respawn` | Respawn and return windows are tick based | stable | Fighters now traverse `FighterId::ALL`; simultaneous final-stock application order is canonical | FIXED (mixed P) |
| Respawn | `fighter::refill_depleted_practice_health` (native) | No countdown; selected dev fighter only | stable fighter ID | Respawn tuple is now chained after ringout, freezing the boundary result; exclude this dev rule from online authority | FIXED, dev-only |
| TickEnd | `canonical_state::canonicalize_authoritative_state` | No countdown; rounds iterative gameplay floats to the 1/4096 grid | stable | Every mutation is field-local; dynamic query requires `StableSimEntity` | PASS |
| TickEnd | `sim_event::commit_sim_event_tick` | Commits current integer tick | none | Archives the already ordered bounded event buffer | PASS |
| TickEnd | `interpolation::capture_sim_pose_tick` | No timer | boundary | Per-fighter pose capture; query order is irrelevant | PASS |

## Changes frozen by this audit

The following are now canonical gameplay rules, not incidental Bevy behavior:

1. Equal-distance aim assist selects the lowest `FighterId`.
2. Equal-distance bot targeting selects the lowest `FighterId`; bot avoidance and
   item inspection traverse dynamic sources by `SimEntityId`.
3. Fighter body-separation pairs and their floating correction reductions use
   ascending `FighterId` order.
4. Same-tick ring-outs and knockouts are collected into a fixed fighter-indexed
   batch and committed in ascending victim `FighterId` order. Attacker validity
   is sampled before the batch, so every valid credit commits before the result
   is calculated; mutual final-stock loss produces a draw.
5. The native practice refill runs after ringout. On the boundary tick, a damaged
   out-of-bounds practice fighter enters `RingOut` before the refill helper can
   restore health.

Fixtures:

- `aim_assist_equal_distance_tie_uses_fighter_id_when_entity_order_is_reversed`
- `equal_distance_bot_target_uses_fighter_id_when_entity_order_is_reversed`
- `bot_dynamic_sources_use_stable_id_when_entity_order_is_reversed`
- `fighter_separation_uses_fighter_pair_order_when_entity_order_is_reversed`
- `simultaneous_final_stock_batch_draws_and_credits_from_pre_batch_snapshot`
- `simultaneous_final_stock_ringouts_draw_credit_both_and_ignore_entity_order`
- `simultaneous_final_stock_loss_is_a_draw_in_every_fighter_entity_order`
- `respawn_stage_runs_ringout_before_practice_refill_on_the_boundary_tick`
- `rollback_replay_event_stream_is_independent_of_excluded_presentation_state`
- `every_authored_aftermath_tuple_reconstructs_exact_cue_and_unknown_fails_closed`
- `frozen_special_shockwave_is_independent_of_target_and_source_ecs_allocation_order`
- `frozen_chick_hazard_is_independent_of_target_and_source_ecs_allocation_order`
- `frozen_penguin_shockwave_is_independent_of_target_and_source_ecs_allocation_order`

Existing stable-relationship fixtures also cover reversed local allocation for
grab release and ultimate lock resolution.

## Required architectural follow-up

### 1. Contact collection and arbitration

The bounded, allocation-free `ContactBuffer` and central
collect/sort/arbitrate/apply pipeline now cover fighter/item hitboxes, generic
specials, Bee/Chick/Penguin abilities, moving items and geometry-only barrel
status, cannon ordnance, and persistent arena hazards. Dynamic generations and
typed static-hazard indices fail closed. Every source consumes outcomes only after
the frozen batch resolves. Simulation version 3 introduced the resulting trade,
reaction, guard, status, claim, source-lifecycle, and event-order behavior;
simulation version 4 preserves it while changing the deterministic math contract.
Simulation version 5 preserves both contact arbitration and the v4 math contract
while changing only the versioned local aim/grab interpretation described above.

Focused fixtures cover fighter trades/reaction/guard/grab permutations, reversed
generic-special, Bee, Chick, Penguin, and item multi-target sources, cannon
multi-target detonation, and hazard-plus-strike insertion/ECS order. The generic
Special, Chick, and Penguin end-to-end fixtures compare exact accepted targets,
ordered semantic event IDs, source hit memory/lifecycle, and canonical target
state across reversed source/target ECS creation. Remaining work is optional
fixture depth rather than source migration: exact-distance contested pickup,
same-priority cross-class reaction order, combined throw-plus-projectile input
tape, unrelated presentation permutations, and full per-tick snapshot/hash
checkpoints.

The per-tick contact records and outcomes are transient, non-authoritative state.
The cumulative `overflow_total` counter is a process-lifetime runtime diagnostic.
All are intentionally excluded from snapshots, and none can feed gameplay
decisions. The deterministically retained contact set still commits ordinary
canonical source/target state and lifecycle outcomes, so permutation fixtures
compare retained identities, outcomes, resulting snapshot-hashed state, and
semantic event IDs rather than hashing the diagnostic counter itself.

### 2. Remaining presentation extraction from fixed systems

Several fixed systems still contain optional client-side effect or visual work.
These paths cannot feed gameplay, and the production headless composition omits
their assets/resources and does not create presentation entities. Moving the
remaining optional sidecars entirely behind ordered `SimEvent` consumption in
`Update` is boundary cleanup, not a canonical-state or headless blocker.

The concrete mixed-state seams are:

- hitbox scene visuals and their `Entity` owner;
- bee swarm orbiters;
- fighter dust/trails/flashes and combat feedback resources;
- arena burn/saw/lever/warning/spark visuals.

The former loose-item pose seam is closed. `ArenaItem::position` and velocity are
canonical, `update_items` does not require a render component, and
`attach_missing_item_visuals`/`sync_item_visuals` run in Update only. Likewise,
fighters and rollback-owned dynamic combat entities use `SimPosition`, with
render `Transform` attached and synchronized only by client presentation systems.

### 3. Final arbitration fixture acceptance

The old `resolve_hitboxes` transient `Entity` maps are gone; fighter arbitration
uses fixed `FighterId`-indexed slots and every source family is migrated. Final
fixture acceptance still requires the missing permutations/checkpoints listed
above and the same-hardware hot-path benchmark required by `docs/performance.md`.

## Verification commands

Run these after any matrix-affecting change:

```text
cargo check --lib
cargo test --lib entity_order_is_reversed
cargo test --lib respawn_stage_runs_ringout_before_practice_refill_on_the_boundary_tick
cargo test --lib
cargo run
```

Per repository policy, any accepted change to a measured hot path additionally
requires the same-hardware before/after benchmark and baseline decision described
in `docs/performance.md`.
