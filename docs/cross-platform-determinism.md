# Cross-platform Determinism Gate

The repository contains one frozen, production-headless simulation tape at
`headless::tests::cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`.
It boots a version-7 match manifest, commits bounded AFC `InputFrame` values for
both occupied seats, runs the real canonical fixed schedule, and ends through
the normal stock/result rules. It does not use the small input-harness probe.

The checked-in contract is:

| Tick | Canonical hash |
| ---: | ---: |
| 1 | `84cee944e41fdcd9` |
| 120 | `6e19030552720060` |
| 240 | `811dfbafe6b568b1` |
| 360 | `224983e404152d7a` |
| 480 | `38d9a1a44b7ca53a` |
| 600 | `33361084398fda66` |
| 709 (final) | `58c567593bc82e7f` |

The final canonical result is team 1 winning at tick 709. The GitHub Actions
workflow `cross-platform-determinism.yml` is configured to run this exact fixture,
all 18 checked-in read-only versioned behavior tapes, and the compact
authored-content matrix on Linux, Windows, and macOS in both Cargo debug and
release profiles. Changes under `tests/` trigger the same matrix. Workflow
configuration is not a claim that the current release candidate has passed:
attach its successful run before acceptance. The headless test composition itself
creates no window, renderer, audio output, or UI. A mismatch fails at the first
stored checkpoint, behavior observation, content-matrix final hash, or final
result assertion.

The compact matrix uses all ten shipping arenas and distributes all eight
characters, three styles, and four equipment choices across four occupied seats.
Each arena has two deliberately independent branches: a 120-tick branch injects
all four retired generic-special request forms, proves that no stable `Special`
entity is spawned, preserves any accompanying ordinary action, and exercises
authored static-hazard contact; a four-tick branch executes an immediate pickup
of that arena's first authored portable item. Two independently bootstrapped
production Bevy worlds must match on every tick in each branch. Keeping the
branches separate prevents one feature from consuming or displacing another
feature's acceptance input. Their synthetic compatibility identity is fixed, so
debug/release build metadata cannot enter the frozen hashes.

| Arena | Retired-special/hazard final hash | Item final hash |
| --- | ---: | ---: |
| Crown Ring | `365ea128e4b146cd` | `0d34896d626088e4` |
| Split Causeway | `1273bd116dc88b71` | `cbd6c658eedcb864` |
| Sunstone Steps | `235919192a3fd974` | `11c36f66f0f878bb` |
| Crank Yard | `ecf5dbfe9427c420` | `eb195f13237d8952` |
| Vent Spiral | `f301ce42883308cf` | `2c3919cf1fd86798` |
| Bumper Alley | `b7cea6c75c37aa6b` | `f4c166ff18eb285c` |
| Feast Market | `217cf971f436345f` | `3be923d9ce3f8307` |
| Snare Garden | `897c27fcfed032bb` | `e7e0ab653b936ad7` |
| Sky Steps | `a1311a7442f60edb` | `01aac6469a98839b` |
| Powder Keg Court | `46214acb53c51dc4` | `c51f9aeb924b4e33` |

The release-candidate workflow separately runs an ignored 100,000-tick soak over
two independently built production `LiveSimulationDriver`/Bevy worlds. It
compares their canonical hashes every 1,000 ticks and at the final tick. The
older `ToyWorld` rollback soak remains useful unit coverage but does not satisfy
this production-state release gate.

The 2026-07-24 arena hierarchy correction was presentation-only, but it changed a
source path included by `build.rs::GAMEPLAY_SOURCES`. The compiled content
identity in all 17 production-builder behavior tapes then present therefore
changed, and their hashes were deliberately refreshed after semantic review. Their checkpoint
observations, event ticks, final ticks, and results did not change. The stock-tape
and compact-matrix tables in this document use fixed synthetic compatibility
identities, so that content-identity-only refresh does not alter the literals
above.

The v6 refresh changes both the simulation discriminator and the fixed-width
fighter payload because snapshot schema 3 adds rollback-owned manual aim state.
BF029 freezes the new acquire/break/release behavior and restores from an active
lock. The other 17 tapes retained identical normalized checkpoints, ordered
events, final ticks, and final results. Debug and release produced the same new
BF001 tick-1 hash (`c50b6cd168b8e793`) before the corpus was refreshed. The
compiled gameplay-content digest is
`5ba689783932ee2cd23cfd0dee6fd7e5fdf366ce3b07f07724c00ae643f21fed`.

The subsequent browser-integration presentation batch centralized menu/tutorial
fades and pause ownership and added persisted audio-channel settings. Simulation
still advances exclusively on the existing fixed schedule; transition animation
uses `Time<Real>` and audio gain synchronization is presentation-only. The
conservatively classified `game_state.rs` and `tutorial.rs` sources changed, so
the gameplay-content digest became
`4253817efe2881ce03d537ba37a8f7f658c823173b3cb60d89019c1f370646b6`.
All 18 normalized checkpoint sets, ordered event ticks, final ticks, and final
results remained identical. Debug and fat-LTO release independently produced
BF001 tick-1 hash `f4e0979e6049e2af` before the identity-derived hashes were
accepted.

The simulation-v7 refresh retires shared specials while retaining the legacy
wire bit. BF013 and the compact matrix inject those old requests and prove they
cannot allocate a special stable ID, start a special cooldown, or emit a special
ability lifecycle event. BF013 alone changes semantic output; all other behavior
tapes retain their normalized checkpoints, ordered events, final ticks, and
results before the identity refresh. Debug and fat-LTO release agreed on BF001
tick-1 hash `cf49d1dde67d32a9`. The compiled gameplay-content digest is
`cde86290adda4918440199f9f5cdb25da3b7ded616dc9b89224f7d7c5ac7bdf6`.

The historical v5 refresh first diverged from the v4 tape at tick 1 because the snapshot
header's canonical simulation-version discriminator changes from 4 to 5. The
stock tape contains no `AIM_GRAB` input, so it is not expected to exercise the
v5 gesture change. The fixture proves that review mechanically: at every
checkpoint and the final state, the then-current fixture rewrote only that
discriminator to recover the historical v4 hashes below. Snapshot schema 3 makes
that old mechanical rewrite inapplicable to v6, so the table is retained as
historical release evidence rather than a current test assertion. The final tick
and team result remain asserted independently.

| Historical v4 tick | Canonical hash |
| ---: | ---: |
| 1 | `0114c86d5060830c` |
| 120 | `57c7c8cab49be405` |
| 240 | `65b92dec2377722a` |
| 360 | `51e4071de3fe06ef` |
| 480 | `e018fe6e389665cd` |
| 600 | `a404842d3686b979` |
| 709 (final) | `a5677c44089653d6` |

Simulation v5 closes the raw `AIM_GRAB` tap compiler gap: aim is held without
grabbing, an inclusive five-tick release emits exactly one grab, holding through
the boundary cancels it, and guard/ultimate priority remains exclusive. BF008
freezes the accepted behavior change; the remaining behavior tapes verify that
the shared compiler does not alter unrelated production-headless behavior. The
full version policy is recorded in
[current-simulation-contract.md](current-simulation-contract.md).

The historical v4 refresh first diverged from the v3 tape at tick 1. This was expected:
the snapshot header now identifies simulation version 4, which is itself canonical
hash input, and the tick runs the new numeric contract. The production
gameplay-content digest separately includes the canonical-math implementation,
direct `libm` contract, and live protocol-input conversion source; this synthetic
golden fixture deliberately uses a fixed content identity. The review retained the
v3 tape's final tick and team result; the checkpoint and final hashes above were
then captured from the intentional v4 contract rather than copied across the
version boundary.

Historical simulation v4 fixed scalar operation order for canonical vector length,
distance, and normalization and uses the software implementation in
`libm = 0.2.16`. Its Q12 vector corpus digest is
`74eb67fd4138faa4`. The Chick ultimate uses a fingerprinted frozen 16-way basis
instead of runtime authoritative trigonometry. Presentation-only pose, camera,
particle, and animation math is outside this contract.

These literals are compatibility data, not snapshots to refresh during routine
refactoring. If an intentional gameplay change modifies them, the change must:

1. bump `CURRENT_SIMULATION_VERSION`;
2. explain the semantic change in `current-simulation-contract.md`;
3. review the first divergent normalized snapshot; and
4. record fresh results from all three CI operating systems.

The production-builder behavior tapes also embed the compiled
gameplay-content identity in every snapshot header. Therefore any edit to a
path in `build.rs::GAMEPLAY_SOURCES` intentionally changes those hashes even
when semantic checkpoints and events do not change. During a multi-file
simulation migration, do not refresh piecemeal: first freeze the source set,
prove debug/release observe identical semantics and the same first hash, then
run the explicit updater once and review the normalized semantic/event diff.

The CI matrix does not replace the physical Steam Deck release test. The Deck
must run the same fixture from the release candidate, and its observed hashes
and final result must be attached to the release record.
