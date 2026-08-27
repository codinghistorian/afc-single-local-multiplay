# Cross-platform Determinism Gate

The repository contains one frozen, production-headless simulation tape at
`headless::tests::cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result`.
It boots a version-10 match manifest, commits bounded AFC `InputFrame` values for
both occupied seats, runs the real canonical fixed schedule, and ends through
the normal stock/result rules. It does not use the small input-harness probe.

The checked-in contract is:

| Tick | Canonical hash |
| ---: | ---: |
| 1 | `12e027215d80758b` |
| 120 | `bed389b627bc18a0` |
| 240 | `ad157a8b1aa63457` |
| 360 | `8e47d2abfa4f1120` |
| 480 | `e94dca2a0b78cf30` |
| 600 | `e6863f856d2434b7` |
| 720 | `aa3e6a52f98b6238` |
| 840 | `78c6457e49b8ee34` |
| 934 (final) | `275c66311d2da33d` |

The final canonical result is team 1 winning at tick 934. The GitHub Actions
workflow `cross-platform-determinism.yml` first runs this same production tape
through `afc-determinism-probe` as Linux native code and through the exported
`afc_determinism_probe_json` function in an actual
`wasm32-unknown-unknown` module under Node. The synthetic manifest, 8
checkpoints, final tick/hash, and winner are serialized as
`afc-linux-wasm-determinism-v1`; CI requires the two JSON files to be
byte-identical. This is executable browser-target simulation evidence, not a
compile-only WASM check or a reduced math surrogate.

The workflow also runs this exact fixture, all 21 checked-in read-only versioned
behavior tapes, and the compact authored-content matrix on Linux, Windows, and
macOS in both Cargo debug and release profiles. Changes under `src/`, `tests/`,
the probe runner, Cargo identity, or the workflow trigger the gate. Workflow
configuration is not a claim that the current release candidate has passed:
attach its successful run before acceptance. The headless test composition itself
creates no window, renderer, audio output, or UI. A mismatch fails at the first
stored checkpoint, behavior observation, content-matrix final hash, or final
result assertion.

The compact matrix uses all eleven shipping arenas and distributes all eight
characters, three styles, and four equipment choices across four occupied seats.
Each arena has two deliberately independent branches: a 120-tick branch injects
all four retired generic-special request forms, proves that no stable `Special`
entity is spawned, preserves any accompanying ordinary action, and exercises
authored static-hazard contact; a four-tick branch executes an immediate pickup
of that arena's first authored portable item, or proves zero pickup when the
arena deliberately authors no item. Two independently bootstrapped
production Bevy worlds must match on every tick in each branch. Keeping the
branches separate prevents one feature from consuming or displacing another
feature's acceptance input. Their synthetic compatibility identity is fixed, so
debug/release build metadata cannot enter the frozen hashes.

| Arena | Retired-special/hazard final hash | Item final hash |
| --- | ---: | ---: |
| Crown Ring | `f8be28ad0a5f644f` | `84dced72a1b1484c` |
| Split Causeway | `bd9ee5fbd8d865f5` | `5019d42b8cfe2d1d` |
| Sunstone Steps | `7268275ccb12b77e` | `2ea15158c4fe8cbf` |
| Crank Yard | `5ab83016a470e0fb` | `0cf0bb2385ee2043` |
| Vent Spiral | `49d91e059bc956c1` | `d3e0d3917a7daff3` |
| Bumper Alley | `238964bdad6723e8` | `980112cff2d3c3c3` |
| Feast Market | `56a35c61641970cd` | `eca79158bca74edf` |
| Snare Garden | `44317b708e1fa3f1` | `fc4eeb964fc337c7` |
| Sky Steps | `ead4d45e0f414db9` | `a110fd1124687bc7` |
| Powder Keg Court | `c2f19381a8eee8cb` | `f6b099bd06555b06` |
| Training Ground | `3f4b7e40d56c181a` | `916f7480d4c27606` (item-free branch) |

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

The additive Training Ground batch adds BF030 and the eleventh compact-matrix
row without changing the simulation discriminator. Canonical collision uses a
four-entry exact-bit barrier table selected by per-world `ActiveArena`; authored
RON float/Euler data is never evaluated by simulation. BF030 freezes the east
wall at Q12 `(34488, 1843, 0)`, zero velocity, grounded/no-stock-loss state, no
events, and restore replay from tick 60. The other 18 behavior tapes retain
identical normalized checkpoints, stable-ID relationships, ordered events,
final ticks, and results. Debug and fat-LTO release produced byte-identical
files and BF001 tick-1 hash `b6e166cd6feadfa6`; the compiled content digest is
`aaf26de55b1f43e4b5a20ac3e50ee39fbc8da9d91317d3403f4bff6f16673b1a`.

Simulation v8 accepts the final Champion's Court and Split Causeway arena flow.
The stock tape now follows the final Crown front apron and retains a team-1
result at the intentional new deciding tick 934. BF031 adds one stable
`ArenaDeviceToggled` event and freezes Split Causeway gate progress at every tick
through the exact 18-tick movement, including restore from tick 10. Snapshot
schema 4 serializes the two target bits and two progress bytes in a bounded
80-byte arena payload. Exact-bit static geometry fingerprints separately cover
35 Crown barriers (`c5499906dcd78474`), 18 Split barriers
(`48cbea118f23e9bc`), and every dynamic gate pose (`cb28c0c31d638d57`).
Debug and fat-LTO release generated a byte-identical 20-file corpus with BF001
tick-1 hash `3eae3ee94c4516d7`; the compiled content digest is
`11f250ab9cc50f8caee1cb34f7cb387c474996b68db84535f4d07b688b214e03`.

Simulation v9 integrates the shared tactical bot planner and makes its latest
accepted fighter-contact context rollback-owned. Snapshot schema 5 appends the
optional contact action and technique plus guarded state for each stable fighter;
the simulation discriminator and conservative bot source/profile digest are also
canonical hash inputs. The 20 production-builder tapes preserved every
normalized checkpoint, stable-ID relationship, ordered event, final tick, and
result while receiving identity-derived hashes; debug and fat-LTO release match
the checked-in corpus, whose BF001 now begins at
`0ff0f4a42dcc0fa3`. The current compiled gameplay-content digest is
`d81201c5b4a2347cb97168bddeb5ba2df237da3b0e3078ccabc206525f42c215`.
The stock tape and eleven-arena matrix above freeze the corresponding synthetic
v9 compatibility identity independently of the production content digest.

The subsequent mixed-controller batch keeps simulation v9, snapshot schema 5,
and the action-level wire format unchanged. Direct controller aim and grab are
separate client-local mask bits that fold into the existing held and pressed
`AIM_GRAB` lanes; controller guard, dash, and ultimate likewise use existing
action-level bits. Conservative source classification changes the production
content digest to
`94d32a0a9666a6f0bff8d7469324aa3b9a905052bc06aef065a95ec94df39a6a` and
BF001 tick 1 to `c54f4e8d05713cb1`. All 20 tapes retain identical normalized
checkpoints, stable-ID relationships, ordered canonical events, final ticks, and
results, and debug plus fat-LTO release reproduce the checked-in corpus. The
synthetic stock and arena-matrix literals above remain unchanged.

Simulation v10 accepts Chick's lower authored stamina costs. BF032 freezes the
ultimate start at tick 1, the delayed raw-heavy start at tick 6, exact Q12
stamina/action state, canonical `ActionStarted` events, and restore from tick
50. The prior 20 production-builder tapes preserve all normalized semantics and
receive only identity-derived hash changes. Debug and fat-LTO release agree on
the 21-file corpus, beginning with BF001 `9e058d015a24d53b`; the compiled
gameplay-content digest is
`ac2b1aca00c903d1650866526c3ef24c0a2470e581403686d3e1a16c2ec0dec2`.
The stock tape and matrix above freeze the synthetic v10 compatibility identity.

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
