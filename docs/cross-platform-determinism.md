# Cross-platform Determinism Gate

The repository contains one frozen, production-headless simulation tape at
`headless::tests::cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`.
It boots a version-5 match manifest, commits bounded AFC `InputFrame` values for
both occupied seats, runs the real canonical fixed schedule, and ends through
the normal stock/result rules. It does not use the small input-harness probe.

The checked-in contract is:

| Tick | Canonical hash |
| ---: | ---: |
| 1 | `5cb79acd3a8477b9` |
| 120 | `fb2fbbca96e50ed0` |
| 240 | `bbee3e67295a0e73` |
| 360 | `8735f2051de7af5a` |
| 480 | `436a842feaed79f0` |
| 600 | `34201466fca40adc` |
| 709 (final) | `dea5b6eb6275a281` |

The final canonical result is team 1 winning at tick 709. The GitHub Actions
workflow `cross-platform-determinism.yml` is configured to run this exact fixture,
all 17 checked-in read-only versioned behavior tapes, and the compact
authored-content matrix on Linux, Windows, and macOS in both Cargo debug and
release profiles. Changes under `tests/` trigger the same matrix. Workflow
configuration is not a claim that the current release candidate has passed:
attach its successful run before acceptance. The headless test composition itself
creates no window, renderer, audio output, or UI. A mismatch fails at the first
stored checkpoint, behavior observation, content-matrix final hash, or final
result assertion.

The compact matrix uses all ten shipping arenas and distributes all eight
characters, three styles, and four equipment choices across four occupied seats.
Each arena has two deliberately independent branches: a 120-tick branch executes
all four generic-special variants plus authored static-hazard contact, and a
four-tick branch executes an immediate pickup of that arena's first authored
portable item. Two independently bootstrapped production Bevy worlds must match
on every tick in each branch. Keeping the branches separate prevents one feature
from consuming or displacing another feature's acceptance input. Their synthetic
compatibility identity is fixed, so debug/release build metadata cannot enter the
frozen hashes.

| Arena | Special/hazard final hash | Item final hash |
| --- | ---: | ---: |
| Crown Ring | `e210c31779aa8715` | `0cea48c846365c7e` |
| Split Causeway | `1c7cddaa5d78d085` | `042421e2fd13c809` |
| Sunstone Steps | `107a0d21ef6a1ee5` | `36baeb3710df9e64` |
| Crank Yard | `64ba5b47f0a15c66` | `ec86de29dfc8f4bd` |
| Vent Spiral | `2d7c86bd46e7ae5d` | `4b0d50bdb65cc76d` |
| Bumper Alley | `763bf0767aef3d90` | `fff3a5f4003634f3` |
| Feast Market | `a438d82d1c479e3b` | `69817e7dd7ab1c81` |
| Snare Garden | `515e09efdf42281d` | `0c6c1b2aa670c364` |
| Sky Steps | `54fdd40e3e45aa68` | `6c18f183855266da` |
| Powder Keg Court | `6ca7d27cfb69210b` | `6097f41c536425a2` |

The release-candidate workflow separately runs an ignored 100,000-tick soak over
two independently built production `LiveSimulationDriver`/Bevy worlds. It
compares their canonical hashes every 1,000 ticks and at the final tick. The
older `ToyWorld` rollback soak remains useful unit coverage but does not satisfy
this production-state release gate.

The 2026-07-24 arena hierarchy correction was presentation-only, but it changed a
source path included by `build.rs::GAMEPLAY_SOURCES`. The compiled content
identity in all 17 production-builder behavior tapes therefore changed and their
hashes were deliberately refreshed after semantic review. Their checkpoint
observations, event ticks, final ticks, and results did not change. The stock-tape
and compact-matrix tables in this document use fixed synthetic compatibility
identities, so that content-identity-only refresh does not alter the literals
above.

The v5 refresh first diverges from the v4 tape at tick 1 because the snapshot
header's canonical simulation-version discriminator changes from 4 to 5. The
stock tape contains no `AIM_GRAB` input, so it is not expected to exercise the
v5 gesture change. The fixture proves that review mechanically: at every
checkpoint and the final state it rewrites only that discriminator to 4 and must
recover the exact historical v4 hashes below. The final tick and team result are
also asserted independently.

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
