# WP0 behavior fixture index

- Status: historical gap index with current implementation addendum
- Audit date: 2026-07-24
- Contract source: [current-simulation-contract.md](current-simulation-contract.md)
- Original scope: tests present before the WP1 schedule cutover

## Current implementation addendum

The opening inventory and individual rows below preserve the pre-cutover audit;
they are not a current claim that fixed scheduling, snapshots, or event routing
are absent. The current production-headless fixture
`headless::tests::cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`
now boots a versioned manifest, applies exact per-tick AFC inputs, executes the
real 60 Hz schedule, stores canonical checkpoint hashes, and asserts the final
stock result. Its Linux/Windows/macOS gate and frozen literals are documented in
[cross-platform-determinism.md](cross-platform-determinism.md). Focused fixtures
also cover the central contact and batched life-loss permutations named later in
this document.

The simulation-v5 fixture tranche is checked in under
`tests/fixtures/behavior/v1` and runs through the crate-internal production
headless harness in `tests/support/behavior_fixtures.rs`:

| ID | Fixture | Versioned tape observations |
| --- | --- | --- |
| BF001 | `move_ground_accel_stop` | Held/released cardinal movement, acceleration/stop poses, per-tick hash |
| BF002 | `move_air_control_land` | Jump plus directional air control through a natural landing |
| BF003 | `jump_tap` | Complete raw jump tap, airborne interval, natural landing |
| BF004 | `dash_gesture_and_motion` | Raw double-tap boundary beside an action-level dash; held motion, trail cadence, inertial release, recovery, and restore replay |
| BF005 | `light_combo` | Raw light edges, delayed action pulses, authored action events |
| BF006 | `heavy_charge_release` | Raw held/released heavy, charge/action/stamina evolution |
| BF007 | `guard_hit` | Natural convergence, held guard, guarded strike contact |
| BF008 | `aim_grab_short_tap` | **AcceptedChange:** held aim, in-window release, one grab pulse/relationship, noisy and post-boundary releases suppressed |
| BF013 | `generic_special_variants` | Projectile, trap, shockwave, and hazard spawn/lifecycle inputs |
| BF015 | `arena_hazard_contact` | Vent inactive interval, impact, hitstop freeze/resume, neutral attribution, damage/reaction/cooldown, and restore replay |
| BF021 | `last_stock_match_completion` | Natural outward movement, stock losses, result event and final phase |
| BF023 | `hitstop_decrement_boundary` | Natural strike contact and the positive-one to post-decrement-zero boundary |
| BF024 | `contested_item_pickup` | **UndefinedLegacyOrder:** two equal-distance same-tick pickup requests; exactly one lower-`FighterId` winner |
| BF025 | `simultaneous_respawn_space_conflict` | **UndefinedLegacyOrder:** same-tick stock loss/respawn into one shared spawn, followed by canonical separation |
| BF026 | `grab_escape_lockout_timeout` | Escape, rejected regrab during lockout, lockout expiry, a second grab, timeout cleanup, and resulting credit |
| BF027 | `quick_directional_heavy_throw` | Same-tick quick and directional-heavy throws with stable order, damage, reaction, knockback, attribution, and relationship cleanup |
| BF028 | `item_use_throw_impact_respawn` | Pickup, apple use, turkey throw/impact, ownership, durability, telemetry, and respawn lifecycle |

Every tape stores a hash for every tick, bounded normalized checkpoints, ordered
semantic events, and final result. The runner compares two clean runs, a
same-world snapshot restore replay, and a fresh presentation-perturbed world. Raw
gesture scripts are compiled before all four runs. The explicit ignored updater
is documented in [current-simulation-contract.md](current-simulation-contract.md);
ordinary tests are read-only.

On 2026-07-24, a presentation-only powder-cannon bomb-parent visibility fix
changed the conservatively defined gameplay-content digest from
`940ffd1093dd6b02df5413b80aa8b8447e0987821fe585c0297ae0c514a8b629`
to
`b0962f667795d7f2d530bdf3b7606b4a361f9f81c11172f6c3c021983efd9d9c`.
Because that digest participates in canonical identity, all 17 tapes received new
per-tick hashes. Their checkpoint counts, semantic-event ticks, final ticks, and
results were unchanged. This was a content-identity-only refresh: simulation
version 5 did not change and no gameplay-result change was accepted.

That broad fixture does not make every Partial/Missing preservation row complete.
The remaining labels describe behavioral depth still useful for regression
coverage, not a missing multiplayer composition boundary. `simulation.rs` is now
registered and its tests run under the normal library test command.

This index maps the high-risk behavior fixtures required by the current simulation
contract to exact Rust test names. It distinguishes a focused unit assertion from
the version-controlled input-tape fixture required by WP0.

At the time of the original audit, no test was a complete contract fixture in the format specified by
`current-simulation-contract.md`: manifest, fixed 60 Hz input frames, checkpoint
ticks, normalized state, ordered events, classification, and allocation-order
permutations. Existing tests are useful anchors, but a **Partial** row must not be
treated as permission to change uncaptured behavior. **Missing** means there is no
test that executes the required behavior end to end.

`simulation::tests::global_tick_advances_while_gameplay_is_frozen_by_hitstop` was
present in `src/simulation.rs` but not registered in `src/lib.rs` at the time of
the original audit. That integration gap is now closed.

## Coverage summary

| High-risk category | Current status | Main gap |
| --- | --- | --- |
| Movement | Partial | BF001/BF002 lock representative ground and air paths; the wider support, camera, style, and landing-boundary matrix remains |
| Jump | Partial | BF003 locks one raw-edge takeoff-to-natural-landing path; broader jump/action and terrain boundaries remain |
| Dash | **Covered for the named v5 fixture** | BF004 locks raw and action-level initiation, held motion, trail cadence, inertial release, recovery, and restore |
| Combo and charged attacks | Partial | BF005/BF006 lock representative input and action evolution; the complete hitbox/contact boundary matrix remains |
| Guard and counter | Partial | BF007 locks guarded contact; both sides of the perfect-counter boundary and the combined counter-contact path remain |
| Grab | **Covered for the named v5 fixture** | BF026 locks escape, regrab lockout, expiry, second grab, timeout, cleanup, and credit |
| Throw | **Covered for the named v5 fixture** | BF027 locks same-tick quick and directional-heavy throws; BF026 also locks the timeout cleanup path |
| Items | Partial | BF024 freezes contested pickup and BF028 covers representative use/throw/impact/respawn; the complete item-role catalog remains |
| Generic and character specials | Partial | BF013 covers the generic variants; generated per-kind character-skill lifecycle breadth remains |
| Arena hazards and devices | Partial | BF015 locks one vent contact lifecycle; the full hazard/device catalog and pipe/cannon timelines remain |
| Ring-out | Partial | Bounds, stock loss, and one lifecycle scenario exist; score/event ordering is incomplete |
| Respawn | Partial | BF025 freezes simultaneous shared-spawn placement/separation; the hitstop matrix remains |
| Match completion | Partial | BF021 locks a representative final-stock result; timed completion and complete confirmed-counter coverage remain |
| Simultaneous/contested interactions | Partial | BF024/BF025 are full tapes and focused reverse-allocation tests cover central contact/life-loss cases; remaining named combinations are listed below |
| Hitstop timing | Partial | BF023 plus production direct tests cover core boundaries; the complete phase/counter matrix remains |

## Required preservation fixtures

### Movement, jump, and dash

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `move_ground_accel_stop` | BF001 drives held then released cardinal movement through the production fixed schedule and records position/velocity/facing, grounded state, stop poses, per-tick hashes, and restore replay. Camera-yaw and style-profile focused tests retain their authored variants. | **Partial.** The named tape is present, but the wider camera, style, support, and movement-direction matrix is not generated. |
| `move_air_control_land` | BF002 applies directional input through jump, airborne control, and a natural landing while recording canonical poses and restore replay. Gravity, landing-stick, support, and wall focused tests retain boundary diagnostics. | **Partial.** One production path is frozen; support shapes, styles, and landing-boundary permutations remain. |
| `jump_tap` | BF003 records one complete raw jump tap, airborne interval, and natural landing with per-tick hashes and restore replay. Coyote-time and jump-attack queue tests separately lock their special cases. | **Partial.** The named base path is frozen, but coyote, buffered-action, and terrain variants are not one generated tape matrix. |
| `dash_gesture_and_motion` | BF004 compares a raw double-tap at the fixed-tick recognition boundary with an action-level dash. It records both held motion paths, authored dash-trail events, release into inertial motion, recovery, per-tick hashes, and a restore inside the lifecycle. The focused dash tests retain their boundary diagnostics. | **Covered by a full v5 input-tape/hash fixture.** This is coverage of the named preservation path, not every character/style dash variation. |

### Attacks, combos, guard, grabs, and throws

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `light_combo` | BF005 records raw light edges, delayed action pulses, authored action events, per-tick hashes, and restore replay. The buffered-chain and authored move-catalog tests retain early-queue and roster variants. | **Partial.** The representative tape does not constitute an early/valid/late contact matrix for every roster move. |
| `heavy_charge_release` | BF006 records raw hold/release, charge/action/stamina evolution, per-tick hashes, and restore replay. Pig charge-cap, release, and authored swing tests retain focused boundaries. | **Partial.** Contact and startup/active/recovery boundaries are not generated across charged techniques and roster variants. |
| `guard_hit` | BF007 records natural convergence, held guard, guarded strike contact, canonical state/events, per-tick hashes, and restore replay. Facing, guard-pressure, depletion ordering, and maximum-duration tests retain focused boundaries. | **Partial.** The representative guarded contact is present, but perfect-counter boundary and complete reaction/feedback permutations remain. |
| `perfect_guard_counter` | `fighter::tests::guard_counter_window_ticks_and_expires`: source and buffer persist while positive, then clear at expiry. `fighter::tests::guard_counter_starts_from_light_or_heavy_and_spends_health`: a valid raw attack begins `GuardCounter`, selects its technique, pays health, clears the window, and faces the source. `fighter::tests::guard_counter_rejects_chord_and_low_health`: a guard chord and insufficient health are rejected. `fighter::tests::heavy_followup_pressed_during_hitstop_is_preserved`: one hitstop input buffer survives into later chain interpretation. | **Partial.** There is no contact-generated counter window fixture at the included and excluded boundary ticks, no resulting counter contact, and no full guard-hitstop interaction. |
| `grab_hold_escape` | BF026 begins with a canonical holder/victim relationship, records victim escape, rejects a regrab during lockout, crosses lockout expiry, creates a second grab, and records timeout cleanup and credit through restore replay. Reverse-allocation contact tests separately lock contested claim creation and strike interruption. | **Covered by a full v5 input-tape/hash fixture.** Other simultaneous claim permutations remain in the contested table. |
| `grab_throw_directions` | BF027 executes a quick throw and a movement-directed heavy throw on the same tick and records stable event order, damage, reaction, knockback, attribution, and both relationship cleanups. BF026 covers the timeout cleanup path. Focused edge-pressure and bracing tests retain profile-level boundaries. | **Covered by a full v5 input-tape/hash fixture plus the BF026 timeout path.** Character/profile catalog breadth remains focused-test coverage. |

### Items and randomness

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `item_pickup_use_throw` | BF028 records two pickups, direct apple use, a turkey throw and same-tick impact, ownership/durability transitions, item-hit telemetry, and the respawn-to-loose boundary. BF024 separately locks equal-distance pickup. `items::tests::thrown_projectile_freezes_all_targets_and_consumes_durability_once` retains the reverse-allocation multi-target invariant. | **Partial.** The complete item-role catalog is not one generated tape matrix; BF028 is representative lifecycle coverage, not a claim about every item kind. |
| `mystery_crate_rng_cutover` | `headless::behavior_fixtures::mystery_crate_reward_is_seeded_and_repeatable` drives a real authored crate through the production headless schedule: seed 1 repeats the same reward and seed 2 selects a different named-stream reward. | **Covered at production-headless test level.** It is not a checked-in golden tape and does not preserve obsolete wall-time output as a current rule. |
| `bot_rng_cutover` | `headless::behavior_fixtures::authority_headless_bot_tape_is_seeded_and_repeatable` records the production authority bot input/action trace twice for one seed and proves selected input and action divergence for a second seed. `bot::tests::bot_choice_hash_is_replay_seeded_tick_keyed_and_purpose_isolated` locks purpose isolation. | **Covered at production-headless/focused-test level.** It is not a checked-in golden tape. |

### Specials, skills, hazards, and arena devices

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `generic_special_variants` | BF013 casts the generic projectile, trap, shockwave, and hazard variants and records their spawn/lifecycle input path, hashes, and restore replay. Activation/profile/repeat/radius and multi-target collector tests retain focused boundaries. | **Partial.** The tape is representative; it is not a generated contact/expiry/despawn matrix across every authored variant and target outcome. |
| `character_skill_lifecycle` | Existing Bee/Chick/Penguin authored lifecycle tests remain. `bee_skills::tests::frozen_multi_target_projectile_outcomes_ignore_ecs_and_pool_allocation_order` proves all targets freeze before source consumption and the post consumer releases the exact generation under reversed allocation. Every character-skill family now uses the same collector/outcome-consumer boundary. | **Partial.** Representative multi-target lifecycle is covered, but no generated per-kind spawn/update/contact/child-spawn/despawn tape exists for the complete catalog. |
| `arena_hazard_contact` | BF015 holds a fighter in an inactive vent, records the first active neutral-source impact, damage/reaction/cooldown, hitstop freeze and resume, per-tick hashes, and a restore across the lifecycle. `arena::tests::hazard_and_strike_both_land_independent_of_insertion_and_ecs_order` separately locks mixed-source allocation invariance. | **Partial.** The named vent path is a full v5 tape, but the requirement says each hazard boundary; the complete hazard catalog is not yet a generated matrix. |
| `arena_pipe_transit` | `arena::tests::crank_pipe_accepts_a_grounded_fighter_or_descending_jump`: grounded or descending-jump entries are accepted; idle airborne, ascending, and heavy attack are rejected. `arena::tests::crank_pipe_transit_sinks_then_emerges_at_the_other_endpoint`: the sampled pose shrinks/sinks at entry, emerges at the other endpoint, and reaches completion. | **Partial.** Dwell threshold, per-fighter state transitions, action/pose lock, exit cooldown, and interaction with separation/hitstop are not run through `update_arena_pipe_transits`. |
| `powder_cannon_bomb` | `arena::tests::headless_cannon_hit_emits_neutral_impact_without_inline_feedback` and `arena::tests::cannon_projectile_freezes_all_targets_and_ignores_ecs_allocation_order` cover neutral semantic impact, multi-target frozen detonation, stable source consumption, and reversed ECS order. | **Partial.** Alternating cannon selection, exact spawn/first-motion tick, ground-only detonation, and next-fire timer still need one tape. |

### Ring-out, respawn, completion, and reset

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `ringout_respawn` | `fighter::tests::ringout_bounds_use_selected_arena_definition`: arena-specific radial and vertical bounds decide ring-out. `game_state::tests::stock_ringouts_track_elimination_and_match_finish`: ordinary loss decrements stock/awards credit; final loss eliminates and enters Results. `fighter::tests::ringout_respawn_lifecycle_uses_same_tick_delay_and_fixed_return_window`: first update enters RingOut, decrements the respawn timer in that same invocation, loses one stock, and hides; after the delay it restores spawn pose, health, stamina, invulnerability, and visibility in Respawning; after `0.45 s` it returns to Idle with elapsed zero. `game_state::tests::telemetry_tracks_local_match_stats` locks independent telemetry counters. | **Partial but strongest existing scenario.** It uses manually advanced variable `Time`, not a fixture tape, and does not jointly assert score/telemetry attribution, ordered events, exact fixed ticks, or snapshot state. |
| `knockout_deferred_until_land` | `fighter::tests::knockout_resolution_waits_for_airborne_hit_reaction`: airborne hitstun with landing aftermath defers knockout, while settled knockdown does not. `fighter::tests::knockout_resolution_waits_for_pending_ground_bounce`: grounded hitstun still defers while a bounce/knockdown-on-land remains. | **Partial.** No complete damage-to-zero path proves exactly one later life loss after landing. |
| `last_stock_match_completion` | BF021 drives natural outward movement through stock loss, the final result event, final phase, per-tick hashes, and restore replay. Batch/system tests separately lock simultaneous draw, multi-credit, reversed allocation, and canonical deciding-tick behavior. | **Partial.** One final-stock tape is present, but every result shape and confirmed progression/telemetry hook is not a generated end-to-end matrix. |
| `timed_match_completion` | `game_state::tests::match_phase_advances_from_countdown_to_results`: consuming `MATCH_SECONDS` enters TimeUp, then `TIME_UP_SECONDS` enters Results, and returning to Setup clears reset request. `game_state::tests::match_timer_continues_advancing_during_hitstop`: with `100 ms` delta, match timer changes `10.0 -> 9.9` while hitstop changes `0.5 -> 0.4`. | **Partial.** No exact 60 Hz boundary tape, `SimTick`, input history, time-up/result event order, or per-tick hash exists. |
| `rematch_full_cleanup` | No test creates a rematch with live chick skills, pipe transit, arena hazard/crank/cannon state, cannon bombs, held/grab/ultimate links, and presentation entities. | **Missing.** The known legacy survivors and the target `AcceptedChange` full reset are not captured. |

## Required hitstop fixtures

| Contract fixture | Existing exact tests and invariant currently locked | Coverage decision |
| --- | --- | --- |
| `hitstop_decrement_boundary` | BF023 records a natural contact, positive remaining hitstop, the positive-one to post-decrement-zero boundary, frozen action state, resume, and restore replay. The focused clamp test retains the local boundary diagnostic. | **Covered by a full v5 input-tape/hash fixture.** The broader counter matrix is a separate row. |
| `hitstop_trigger_mid_step` | `headless::behavior_fixtures::action_phase_hitstop_trigger_freezes_movement_and_later_item_motion_same_step` triggers hitstop at Action end in the production schedule and proves Movement and later moving-item work freeze immediately. | **Covered at production-headless direct-test level.** It is not a checked-in golden tape. |
| `hitstop_match_clock_runs` | `game_state::tests::match_timer_continues_advancing_during_hitstop` locks match-time advance. The now-registered `simulation::tests::global_tick_advances_while_gameplay_is_frozen_by_hitstop` locks global tick advance while guarded gameplay work freezes. | **Covered for the core clock/tick rule at focused-test level.** A single catalog-wide counter matrix remains absent. |
| `hitstop_counter_matrix` | `fighter::tests::heavy_followup_pressed_during_hitstop_is_preserved` proves one permitted action buffer. No test covers equipment cooldown, pipe, separation, loose/respawn item timers, respawn, special cooldown, movement, hazards, and moving items together. | **Missing as a matrix fixture.** |
| `hitstop_existing_hitbox_contact` | `headless::behavior_fixtures::pre_existing_hitbox_contacts_new_target_while_phase_remains_frozen` proves an existing hitbox can accept a newly valid target while lifetime/path motion stays frozen in the production schedule. | **Covered at production-headless direct-test level.** It is not a checked-in golden tape. |
| `hitstop_duration_max` | `game_state::tests::overlapping_hitstop_triggers_keep_the_longest_remaining_duration`: `0.12` followed by `0.04` remains `0.12`; a later `0.2` replaces it rather than adding. | **Covered at unit level; scripted tick fixture still absent.** |
| `hitstop_presentation_not_replayed` | `sim_event::tests::presentation_cursor_rewind_reobserves_but_deduplicates_one_shots`, `sim_event::tests::presentation_router_never_replays_deduplicated_events_after_rollback`, and `combat::tests::rollback_resimulation_deduplicates_action_timeline_cues` lock rollback one-shot suppression. | **Covered at presentation-router/focused-system level.** A rendered end-to-end audio/effect/camera capture remains external evidence, not a canonical tape requirement. |

## Required contested and simultaneous fixtures

Focused contact-arbitration fixtures now reverse fighter and dynamic-source ECS
allocation across hitboxes, abilities, items, cannon ordnance, and typed static
hazards. They are exact system tests rather than the final input-tape/hash fixtures.
Same-tick life loss is separately covered by the fixed `LifeLossBatch` and its
reversed-allocation system/snapshot fixtures.

| Contract fixture | Existing exact test or diagnostic | Coverage decision |
| --- | --- | --- |
| `trade_two_strikes` | `combat::tests::frozen_hitbox_contacts_allow_a_real_two_strike_trade` freezes both valid contacts before applying either, then proves both damage/events are accepted with reversed fighter and hitbox allocation. | **Covered at focused ECS-system level.** A full input-tape checkpoint/hash fixture remains. |
| `two_hits_one_target` | `combat::tests::strongest_authored_reaction_wins_final_state_after_all_damage_commits` proves both accepted hits commit damage while the strongest authored reaction wins, independent of fighter and hitbox allocation. `combat::tests::guard_depletion_makes_the_later_same_tick_contact_unguarded` covers sequential guard-state dependence in canonical order. | **Covered at focused ECS-system level.** Cross-source permutations and a full input-tape checkpoint/hash fixture remain. |
| `grab_vs_grab` | `combat::tests::competing_grabs_choose_lower_holder_and_one_role_cannot_chain_claims` proves a fighter cannot be both victim and a second holder in the same arbitration tick. `combat::tests::cinematic_catch_claim_arbitrates_before_ordinary_grab` proves cinematic catch precedence over ordinary grab. | **Covered at focused ECS-system level.** Full input-tape/hash coverage remains. |
| `two_grabbers_one_victim` | `combat::tests::two_grabs_for_one_victim_choose_the_lower_holder_id` proves the lower stable holder ID wins and exactly one claim is accepted with reversed fighter/hitbox allocation. | **Covered at focused ECS-system level.** Full input-tape/hash coverage remains. |
| `two_fighters_one_item` | BF024 places two fighters symmetrically around one stable item, compiles the same delayed light pulse for both, and freezes the single lower-`FighterId` winner through clean, perturbed, and restore executions. | **Covered by a full v5 input tape/hash fixture.** |
| `ultimate_lock_conflict` | `fighter::tests::pig_ultimate_lock_lasts_until_heavy_finisher` only locks release timing (`Cat = 0.9 s`, Pig greater than `1.18 s`). It does not exercise competing relationships. | **Missing.** |
| `hazard_and_strike_same_step` | `arena::tests::hazard_and_strike_both_land_independent_of_insertion_and_ecs_order` proves both contacts, outcomes, event sources, health, and hazard cooldown are invariant. | **Covered at focused ECS-system level.** Same-priority final-reaction and snapshot/hash assertions remain. |
| `throw_and_projectile_same_step` | Item and cannon multi-target fixtures independently prove frozen target sets and one source consumption under reversed allocation. | **Partial.** A combined item-throw plus special-projectile tick must still assert canonical order, max hitstop, attribution, snapshot, and hash. |
| `simultaneous_ringouts` | `game_state::tests::simultaneous_final_stock_batch_draws_and_credits_from_pre_batch_snapshot` inserts losses in reverse order, commits by `FighterId`, and awards both valid attackers from the pre-batch participation snapshot. `fighter::tests::simultaneous_final_stock_ringouts_draw_credit_both_and_ignore_entity_order` proves the complete ring-out system result and event order are allocation independent. | **Covered at focused batch/system level.** A full input-tape checkpoint/hash fixture remains. |
| `final_stock_trade` | The same batch/system fixtures prove `[0, 0, 0, 0]` becomes a draw after both credits commit. `live_match_snapshot::tests::simultaneous_final_stock_loss_is_a_draw_in_every_fighter_entity_order` locks the canonical `MatchResultSnapshot::Draw` and deciding tick. | **Covered at focused batch/snapshot level.** A combined same-tick strike-to-KO input tape and confirmed result-ID assertion remain. |
| `respawn_space_conflict` | BF025 moves two non-eliminated fighters out of bounds on the same tick, gives them one shared respawn point, freezes both respawn events on one tick, and proves later canonical body separation. | **Covered by a full v5 input tape/hash fixture.** |
| `pool_capacity_overflow` | `contact_arbitration::tests::capacity_overflow_retains_same_canonical_set_for_every_permutation` proves the fixed-capacity contact buffer retains the same canonical contacts under every insertion permutation and counts overflow. `penguin_skills::tests::ice_trail_cap_despawns_oldest_segments_per_owner` proves one local cap helper selects the oldest same-owner segment. | **Partial.** Contact-buffer overflow is covered; authoritative entity-pool generation reuse and full-pool reject/drop policy are still not exercised. |

## Remaining evidence before the historical fixture gate is exhaustive

1. Extend the checked-in corpus across the preservation rows still marked Partial
   or Missing, especially the perfect-counter boundary, character-skill catalog,
   pipe/cannon devices, knockout/timed completion, and full rematch cleanup.
2. Generate catalog matrices for every item role and arena hazard. BF028 and BF015
   are representative lifecycle tapes, not exhaustive catalog claims.
3. Add the combined hitstop counter matrix covering every advancing and frozen
   subsystem in one production-schedule fixture.
4. Complete the remaining contested combinations, including ultimate-lock
   conflict and throw-plus-projectile, and promote focused system coverage to
   full input tapes where the contract requires allocation/presentation
   permutations.
5. Retain `UndefinedLegacyOrder` for BF024/BF025 and any future differing legacy
   result; never bless an observed ECS order without a named arbitration key.
6. Continue storing semantic assertions before hashes. A content-identity-only
   hash refresh must prove checkpoint counts, event ticks, final ticks, and
   results are unchanged and must not be described as behavior coverage.

This index should be updated in the same change that adds or renames a behavior
fixture. Unit tests may remain as focused diagnostics, but the row should move to
Covered only when the scripted fixture records every required observation from the
contract.
