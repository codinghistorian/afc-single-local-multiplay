# Performance Protocol

Performance changes are accepted only when they improve a repeatable workload and
preserve correctness. Compare builds on the same machine, OS, display settings,
compiler, browser, resolution, scenario seed, and commit configuration.

## Release targets

- Reference display: 1280x720 at 60 Hz.
- Native and current Chrome/Safari web: p99 frame time below 16.67 ms.
- Average measured CPU and GPU work: below 8.33 ms each in the stress scenario.
- Optimization program: at least 30% lower p95 CPU time and 25% lower
  vsync-capped CPU usage relative to the recorded starting baseline.
- No monotonic entity, mesh, material, or resident-memory growth over 100 map
  switches or a ten-minute soak.
- Unchanged HUD frames perform no string allocation or UI component mutation.
- Generated WASM target: at most 61,498,982 bytes (58.65 MiB), 25% below the
  planning baseline of 78.2 MiB.
- Generated WASM guardrail: at most 64,573,931 bytes (61.58 MiB), 5% above the
  target. The web build fails above this value.

## Scenarios

The rendered scenario contract below is the canonical schema-v6 harness. It uses
the normal match-reset lifecycle, promotes four real `Combatant` bot brains only
after the complete logical and rendered fixture is ready, and fences every
fixture on an acquired surface texture that Bevy consumes through
`SurfaceTexture::present`. Historical v1 captures remain superseded: their
training-dummy workload, UI churn, allocator replacement, and unfenced map
completion make them unsuitable as baselines. The never-accepted v2 harness is
also superseded because its post-render system acknowledged unconditionally and
therefore proved render-schedule ordering, not a present invocation.

| Scenario | Workload | Required observations |
| --- | --- | --- |
| `FourBotStress` | Five minutes in Bumper Alley (arena index 5): four seeded `Combatant` bots, four authored items, and three authored/public hazards. | Mean/median/p95/p99 CPU and whole-frame time, separate external GPU and allocation evidence, exact fixture counts, bounded diagnostic resource slopes, and owner activity. |
| `MapCycle100` | Preload exactly 101 supported assets from `arena`, `backgrounds`, and `music/bgm`; present one complete ten-arena warm precycle, then measure 100 presented switches. | Exactly 100 switch samples, 111 present acknowledgements including initial/final fences, 11 aligned cycle checkpoints, exact scene/item/hazard counts, and aligned RSS/live-byte gates. |
| `Soak10Minutes` | Ten uninterrupted minutes with the same fixed Bumper Alley four-bot fixture. | First/last-minute drift, bounded entity/asset/RSS/live-byte slopes, final asset plateau, peak/end stale-owner entities, and a separate external GPU capture. |

Canonical captures use Bevy `AutoVsync`, matching normal gameplay.
`AFC_PERF_UNCAPPED=1` selects `AutoNoVsync`, makes
`canonical_present_mode_eligible:false`, and is exploratory only. Use a
release-equivalent profile:

```bash
AFC_PERF_SCENARIO=FourBotStress \
AFC_PERF_RUN_ID=baseline-1 \
cargo run --profile profiling --no-default-features --features native,perf
```

The automated runner accepts `FourBotStress`, `MapCycle100`, or
`Soak10Minutes`. It uses seed `0x00000000ffc00001`, a 30-second warmup, a
five-minute measurement for stress/map cycling, and a ten-minute measurement for
the soak. `AFC_PERF_SEED`, `AFC_PERF_WARMUP_SECONDS`, and
`AFC_PERF_MEASUREMENT_SECONDS` override those values for a labeled experiment.
It exits automatically after printing one `AFC_PERF_RESULT` JSON record.

Canonical release evidence runs an already-built immutable binary through
`scripts/run_graphical_perf.py`. The runner removes seed, duration, and uncapped
overrides; sets `BEVY_ASSET_ROOT` to the repository root; requires exactly one
matching schema-v6 result; and verifies the executable SHA-256 before and after
every run. It records host, source status, binary architecture, and power at
both edges. On Apple Silicon it rejects a non-arm64 binary, non-AC capture, or
power-source transition. Preserve the same source status for the whole matrix.
For example:

```bash
python3 scripts/run_graphical_perf.py \
  --binary target/perf-captures/release/bin/ffc-prototype-timing \
  --output-dir target/perf-captures/release/timing-fourbot \
  --kind timing --scenario FourBotStress --run-prefix release
```

The timing/RSS build reports whole-frame and main-App CPU mean/percentiles,
first/last-minute drift, process CPU/RSS, fixed four-owner activity, exact
fixture counts, bounded resource slopes, stale-owner counts, assets, and
map-switch latency. Its `AFC_PERF_RESULT` JSON uses `schema_version:6` and
explicitly declares
`allocation_instrumentation:false`; it does not replace the process allocator.
Use a separate allocation diagnostic build when allocation information is wanted:

```bash
cargo run --profile profiling --no-default-features --features native,perf-alloc
```

Before measurement, the harness pre-touches its preallocated frame-duration
buffers so first-touch page faults are outside the sample. Every measured frame
is classified exactly once as steady, transition, or finalization work; schema
v6 requires those three counts to sum exactly to the aggregate frame count with
no gap or overlap. The reported whole-frame p99 is always computed from that
complete aggregate, not from the cheaper steady subset.

The main world publishes present-fixture evidence in `Last`, after gameplay
mutation and before render extraction. That evidence is valid only when its
request epoch and exact arena generation match and the fighter, combatant-bot,
item, and public hazard-marker counts are all exact. Readiness is non-vacuous:
there must be at least one `SceneRoot`, every root must expose a
`SceneInstance`, and every instance must be ready in `SceneSpawner`. Combat bots
are promoted only after this rendered evidence and the complete logical fixture
are simultaneously ready. The render-world fence cannot arm from an epoch
alone. Its arm runs after
`RenderSystems::ManageViews` and immediately before Bevy's
`renderer::render_system`; it requires the matching evidence, an acquired
swapchain surface texture, and the same window-present condition used by Bevy.
It records the window entity and adjacent `TextureViewId`. Bevy's public
`SurfaceTexture` wrapper does not expose a texture identity, so the
acknowledgement deliberately makes the strongest public-API claim available:
after `render_system`, that same window's surface texture is gone while the
recorded view identity is unchanged. This proves that
`ExtractedWindow::present` invoked `SurfaceTexture::present` for that
window/view acquisition. It does not prove GPU completion or scanout; wgpu
executes GPU work asynchronously.

The fixed combat fixtures revalidate their exact epoch, arena generation, and
all four count classes at the final presented frame before output.
`MapCycle100` applies the same evidence to every switch, including the 100th.
Specialized saw-blade and Vent Spiral reactor visuals are covered by the exact
scene-generation identity; they intentionally do not carry
`ArenaHazardMarker`. Bevy 0.18 does not provide Metal GPU timestamps, so a Metal
System Trace remains a separate platform capture. Do not enable
`AFC_PERF_RENDER_DIAGNOSTICS` during timing or allocation baselines because its
per-pass path collection allocates.

`fixture_valid:true` means only that the requested workload and its executable
integrity gates passed: exact fixture counts, render evidence, combat activity,
event-history continuity, owner attribution, map sample completeness, and the
scenario-specific resource-stability checks. It is never a performance-budget
pass. Acceptance status is evaluated in fail-closed evidence order:
`fixture_invalid`, `exploratory_only_noncanonical_configuration`, the timing
budget for an uninstrumented timing build, the scenario's applicable RSS gate,
the allocation build's applicable live-byte gate, and finally
`external_gpu_evidence_required`. Thus a later gate cannot hide an earlier
failure. `render_evidence` is
`same_window_same_view_surface_texture_present_invoked` only after a successful
fence and otherwise is `surface_present_invocation_not_observed`. A locally
accepted capture still reports
`external_gpu_evidence_status:"required_not_collected"` and
`gpu_completion_measured:false`; only an external platform trace can close that
gate.

Resource growth uses a bounded 64-entry sampler: 48 time-density-thinned
historical samples plus the exact latest 16 samples. The first sample and broad
whole-run time coverage survive overrun, while the exact tail preserves the
intermediate samples needed for final-window plateau analysis. Equal-time
observations replace the retained sample at that time. The JSON reports retained
sample count, total observations, history/tail counts, thinning count, and the
bounded series with elapsed time, entity/asset counts, RSS, live bytes, and stale
owners. Per-minute least-squares slopes and final-window range/plateau values are
computed over the representative retained series. Monotonic-growth flags are
tracked separately over every raw observation, so thinning cannot erase an
intermediate dip. Start/peak/end and stale-owner peak/end remain independent
edge/high-water evidence.
For `MapCycle100`, executable validity requires the exact 10-present warm
precycle, 100 measured switch/latency samples, 111 total present
acknowledgements, and 11 aligned cycle checkpoints. Resource counts must match
at every aligned checkpoint. Memory acceptance uses the last
four aligned checkpoints, hence exactly three complete-cycle intervals: RSS
range at most 8 MiB and absolute slope at most 2 MiB/minute in both builds; in
the `perf-alloc` build, live-byte range at most 1 MiB and absolute slope at most
0.25 MiB/minute. Whole-run or wall-clock-tail Map memory fields are diagnostic
and do not replace these arena-cycle-aligned gates.

For the soak, executable validity requires:

- exact asset counts throughout the final 60-second window;
- no strictly monotonic entity, mesh, material, image, or RSS growth;
- a final RSS range at most 8 MiB and absolute final-window RSS slope at most
  2 MiB/minute;
- no stale-owner entity in any sample or at the measurement edge; and
- in the separate `perf-alloc` build, no strictly monotonic live-byte growth,
  a final live-byte range at most 1 MiB, and an absolute final-window live-byte
  slope at most 0.25 MiB/minute.

These are conservative leak/plateau guardrails, not historical performance
claims. Revise them only with same-hardware corrected captures and update the
baseline table. The timing build reports live-byte evidence as
`unavailable_without_perf_alloc`; its allocator result fields are `null` and
never count as a pass. Likewise, a platform on which the process sampler cannot
obtain RSS reports `rss_evidence_available:false` and
`rss_growth_acceptance_status:"unavailable_on_platform"`; a soak cannot pass
with synthetic zero RSS values.

The allocation build uses a generation-tagged four-phase protocol:
inactive, opening, active, and closing. Every allocation, zeroed allocation,
reallocation, and deallocation registers as an in-flight mutator before entering
the system allocator or changing counters. Opening and closing prevent new
mutators from crossing a measurement boundary and wait for already-registered
mutators to drain. A mutator admitted during the active generation retains that
ticket until it has published its captured post-increment live-byte high, even
if another thread frees the allocation first. The ending snapshot therefore
cannot miss delayed peak publication, which is the interleaving that invalidated
the v2 peak capture.

Measurement readiness is explicit. If a run times out before warmup completes,
`measurement_started:false`,
`allocation_measurement_status:"unavailable_measurement_not_started"`, and all
allocation result fields are `null`. Reporting neither ends an allocation epoch
that never began nor subtracts process-lifetime counters from a zero baseline.

All three fixtures reject a nonzero stale-owner peak. This catches a dynamic
combat entity whose owner no longer maps to one of the fixture's active fighter
IDs even if that entity disappears again before the ending sample.

When invoking the already-built executable directly, preserve Bevy's asset root:

```bash
BEVY_ASSET_ROOT="$PWD" AFC_PERF_SCENARIO=FourBotStress \
  AFC_PERF_RUN_ID=baseline-2 target/profiling/ffc-prototype
```

For a Tracy capture:

```bash
cargo run --profile profiling --no-default-features --features native,trace
```

Web measurements use a local `./scripts/build_web.sh` artifact and current Chrome
and Safari. Record the optimized WASM and total distribution sizes printed by the
script.

## Multiplayer acceptance capture

The render-frame scenarios above do not establish the multiplayer budgets in
[multiplayer-architecture.md](multiplayer-architecture.md). Build and run the
separate render-free profiler for authority, rollback, and simulation-allocation
evidence:

```bash
cargo run --profile profiling --no-default-features --features perf-alloc \
  --bin afc-multiplayer-profile -- \
  --hardware "Apple M2 Max, 12-core CPU, 32 GiB, macOS 26.5.1" \
  --run-id "$(git rev-parse --short HEAD)-run1"
```

The hardware stamp is required. Include the exact CPU, RAM, OS version, power
mode, and any fixed CPU-affinity or background-load policy used for every
comparison. Keep the compiler/toolchain, profiling profile, seed, and commit
configuration identical. Run the acceptance capture at least three times on
each minimum supported CPU and retain every `AFC_MULTIPLAYER_PERF_RESULT` JSON
line with the commit artifacts. Use `--report-only` only for exploratory capture:
without it, the executable exits nonzero if an acceptance budget is missed.

The profiler drives the production `AuthorityMatch::step` path, including bot
input commit, the canonical fixed schedule, canonical snapshot capture, and
state hashing. Its rollback sample is one real late-input correction at exactly
12 ticks of depth through the production predicted `LiveSimulationDriver`; the
twelve prediction ticks used to establish each fixture are outside the timed
and allocation-counted correction window. Duration storage is preallocated, and
the existing process-wide counting allocator is sampled immediately outside
each measured operation. The result includes p50/p95/p99/max nanoseconds,
allocation counts/bytes, configured rollback depth, and history high-water
marks. The acceptance gates are:

- authority p99 below 1,000,000 ns;
- exact 12-tick rollback p99 below 4,000,000 ns;
- zero allocations in measured steady-state authority steps;
- normal rollback never above the manifest's 12-tick cap; and
- authority, rollback-snapshot, and rollback-input histories at or below their
  fixed capacities.

The result also reports allocations inside each rollback correction. Those are
diagnostic because the architecture's zero-allocation acceptance target is
defined for a steady-state simulation tick; they must still be reviewed before
accepting rollback optimization work.

Packet, bandwidth, and transport-queue acceptance remain owned by the network
codec/laboratory rather than this CPU profiler:

```bash
cargo test --lib network_codec::tests::packet_and_output_limits_are_enforced
cargo test --lib network_lab_tests -- --nocapture
cargo test --lib listen_authority::live_network_acceptance -- --nocapture
```

The codec enforces the 1,200-byte high-frequency packet ceiling. The laboratory
meters bytes from actual encoded datagrams per direction/channel, evaluates the
16 KiB/s upstream and 64 KiB/s downstream averages, and asserts transport,
runtime, rollback, and history high-water bounds. The production-live acceptance
module supplements that matrix with real headless authority/client composition.
See [network-lab-acceptance.md](network-lab-acceptance.md) for scenario duration
and fault details.

| Multiplayer budget | Executable evidence | Current same-hardware status |
| --- | --- | --- |
| Authority 60 Hz step p99 below 1 ms | `afc-multiplayer-profile` production headless authority sample | Canonical-pose developer reference accepted at 58,667 ns median-of-nine p99; minimum-supported-CPU capture remains pending |
| Exact 12-tick rollback p99 below 4 ms | `afc-multiplayer-profile` production correction/resimulation sample | Canonical-pose developer reference accepted at 403,791 ns median-of-nine p99 and exact depth 12; minimum-supported-CPU capture remains pending |
| Zero steady-state simulation allocations | Counting-allocator deltas around each authority sample | Canonical-pose developer reference accepted at zero allocations in all nine 1,000-step runs |
| High-frequency packet below about 1,200 bytes | `network_codec` fixed-buffer encode/decode limits and tests | Executable, hardware-independent contract |
| Upstream/downstream averages at most 16/64 KiB/s | `network_lab_tests` metered encoded datagrams | Executable scenario evidence; rerun for release artifacts |
| Bounded history and queues | Profiler history high-water plus network lab and production-live queue bounds | Executable bounds; no monotonic-growth claim without a completed run |

Do not add a timing row to the baseline table below from a debug build, a run
with fewer than 100 samples, or an unstamped machine. Replace a pending entry
only after the same executable and seed reproduce the result.

## Baseline record

| Date | Hardware and platform | Scenario/build | Result | Status |
| --- | --- | --- | --- | --- |
| 2026-07-22 | Apple M2 Max, macOS | Existing generated WASM | Approximately 78.2 MiB before the optimization program | Planning baseline; replace with exact scripted result. |
| 2026-07-23 | Apple M2 Max (12 physical cores, 32 GiB), macOS 26.5.1, Metal, 1280x720 | Legacy v1 `FourBotStress`, profiling profile, three seeded runs | Median run: whole frame 3.670/15.252/21.959 ms median/p95/p99; main-App CPU 2.085/2.535/2.681 ms; 525 entities; 131 meshes; 114 materials; 36 images. | Superseded diagnostic only: bot brains remained `TrainingDummy`, Start/hidden UI churn was present, and allocator instrumentation perturbed timing. |
| 2026-07-23 | Apple M2 Max (12 physical cores, 32 GiB), macOS 26.5.1, Metal, 1280x720 | Legacy v1 pre-contact/presentation `FourBotStress`, immutable profiling executable, three seeded runs | Median whole-frame p95: 13.577 ms (run 2); median main-App CPU p95: 3.016 ms; 511 entities; 131 meshes; 114 materials; 36 images. | Superseded diagnostic only for the same invalid workload; not an accepted pre/post combat baseline. |
| 2026-07-23 | Apple M2 Max (12 physical cores, 32 GiB), macOS 26.5.1, Metal, 1280x720 | Legacy v1 `MapCycle100`, profiling profile, three seeded runs | Median whole-frame-p95 run: whole frame 3.940/15.064/20.395 ms; main-App CPU 1.901/2.551/3.058 ms; reported switch 3.435/5.213/5.496 ms median/p95/p99; peak/end entities 1,448/826. | Superseded diagnostic only: completion was counted in the next `Last` schedule without proving scene instantiation or one rendered frame. |
| 2026-07-23 | Apple M2 Max (12 physical cores, 32 GiB), macOS 26.5.1, Metal, 1280x720 | Legacy v1 `Soak10Minutes`, profiling profile, one seeded ten-minute run | Whole frame p95 first/last minute 14.980/16.006 ms; main-App CPU p95 2.608/2.642 ms; RSS start/end/peak 0.289/0.354/0.354 GiB; entities/assets stable. | Superseded cache-growth signal, not a combat soak or proven leak: bot brains were idle and tracked live bytes grew 56.86 MiB without an owner/slope plateau diagnosis. |
| 2026-07-23 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, battery power, High Power mode, no fixed affinity | Deterministic-math C1, immutable `afc-multiplayer-profile`, three 1,000-sample runs before and after | Accepted post-C1 median p99: authority 62,209 ns, exact 12-tick rollback 378,708 ns; respectively -0.3% and -3.1% versus 62,417/390,625 ns before. Zero authority allocations and all history/depth gates retained. | Accepted same-hardware C1 hot-path baseline. This is a developer-reference machine, not a substitute for minimum-supported-CPU release evidence. |
| 2026-07-23 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, AC power, High Power mode, no fixed affinity | Simulation-v4 canonical normalization, interleaved immutable C1/v4 `afc-multiplayer-profile` runs, three 1,000-sample pairs | Accepted v4 median p99: authority 53,292 ns, exact 12-tick rollback 363,708 ns; respectively -1.4% and +2.1% versus the same-power C1 values 54,042/356,083 ns. Zero authority allocations and all history/depth gates retained. | Accepted same-hardware simulation-v4 hot-path baseline. The rollback change is immaterial against the 4 ms budget; minimum-supported-CPU evidence remains required. |
| 2026-07-23 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, battery power, High Power mode, no fixed affinity | Final simulation-v5 gameplay-source set, interleaved immutable pre/post `afc-multiplayer-profile` executables, nine 1,000-sample pairs | Accepted v5 median p99: authority 63,583 ns, exact 12-tick rollback 402,125 ns; respectively +2.2% and +3.1% versus 62,208/390,125 ns before. Zero authority allocations, identical rollback allocation diagnostics, and all history/depth gates retained. | Accepted same-hardware simulation-v5 hot-path baseline. Both changes are immaterial against the 1/4 ms budgets; minimum-supported-CPU evidence remains required. |
| 2026-07-24 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, AC power, High Power mode, no fixed affinity | Canonical `SimPosition` ownership and render-only `Transform` projection, interleaved immutable v5/canonical-pose `afc-multiplayer-profile` executables, nine 1,000-sample pairs | Accepted canonical-pose median p99: authority 58,667 ns, exact 12-tick rollback 403,791 ns; respectively -1.9% and -1.3% versus the same-power v5 values 59,833/409,292 ns. Zero authority allocations; rollback diagnostics improved from 121,083 allocations / 142,311,564 bytes to 120,083 / 142,279,564; all history/depth gates retained. | Accepted same-hardware canonical-pose hot-path baseline. Both paths remain far inside the 1/4 ms budgets; minimum-supported-CPU evidence remains required. |
| 2026-07-26 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1; local verification invocation whose power edges were not recorded by the profiler | Three `afc-multiplayer-profile` runs (`final-local-01` through `03`), 1,000 authority samples and 1,000 exact 12-tick rollback samples each | Authority p99 66,916–73,166 ns (median 67,292 ns); rollback p99 370,417–376,958 ns (median 376,750 ns). Authority remained allocation-free; rollback diagnostics were identical at 120,083 allocations / 142,279,564 bytes; every timing, depth, and history gate passed. | Accepted local verification, not a new controlled before/after baseline and not minimum-supported-CPU evidence. |
| 2026-07-26 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, Metal, 1280x720, AC power, native arm64 | Schema-v6 pre-backport `MapCycle100`, immutable timing triplicate plus one allocation run | Timing frame/CPU p99 medians 8.992834/2.439333 ms. Allocation run aligned RSS range/slope 2.125000 MiB / 1.402960 MiB/min passed, but aligned live range/slope 1.570396 MiB / 1.052623 MiB/min and +5,752,718 live bytes failed. | Accepted timing evidence; rejected allocation baseline. This same-hardware result identified the render-pass name leak corrected below. External GPU was not evaluated for the failed allocation run. |
| 2026-07-26 | Mac14,6, Apple M2 Max (12-core CPU, 32 GiB), macOS 26.5.1, Metal, 1280x720, AC power, native arm64 | Schema-v6 post-backport full local matrix: timing and allocation `FourBotStress`/`MapCycle100`/`Soak10Minutes` | All 14 admissible captures passed fixture/canonical-mode and every applicable local timing, RSS/live, stale-owner, presentation, and exact-resource gate. Maximum reported frame/CPU p99, including diagnostic allocator timing, was 10.060500/3.847458 ms. Detailed exact values and hashes follow. | Accepted Apple M2 Max local baseline. Every result remains `external_gpu_evidence_required`; minimum-supported-CPU and external GPU captures remain pending. |
| Pending | Minimum native target and Apple M2 Max | Schema-v6 external GPU trace and minimum-supported-CPU capture | Repeat the canonical matrix on the minimum CPU and attach platform GPU-completion evidence for stress and soak. | Required for release acceptance; the local JSON explicitly does not measure GPU completion. |
| Pending | Current Chrome and Safari | Optimized web release | Capture p95/p99 frame time, WASM, and distribution size. | Required before changing the accepted web baseline. |

### Schema-v6 local graphical baseline

The post-backport matrix used one immutable timing executable and one immutable
allocation executable:

- timing SHA-256:
  `9caaa991644f367d772e11a4f7964ec71c25f0b51d496828558b1e2aaed6e7fd`;
- allocation SHA-256:
  `54d6239ec592bf3139f24cfc120abb23ccfbd7115a22e70bec097d7920b49db6`.

The values below are in capture order. For Map, memory is the canonical aligned
range in MiB / absolute slope in MiB/minute over the last four checkpoints.
For Soak, it is the final 60-second window. FourBot memory is diagnostic and is
not an acceptance gate. Only the uninstrumented timing rows own the timing
budget; frame and CPU values from allocation rows are diagnostic.

| Scenario/build | Frame p99 (ms) | Main-App CPU p99 (ms) | Canonical memory evidence | Runner result |
| --- | --- | --- | --- | --- |
| `FourBotStress` timing, 3 runs | 8.961750 / 8.925416 / 8.971791 | 2.461667 / 2.263375 / 2.273792 | Final RSS: 0.031250 / 0.010813; 1.406250 / 1.537059; 0.062500 / 0.028124 | 3/3 locally accepted |
| `FourBotStress` allocation, 3 runs | 9.068042 / 8.935167 / 8.941041 | 2.990708 / 2.333084 / 2.290125 | Final live: 0.059624 / 0.043370; 0.064453 / 0.043081; 0.050821 / 0.031369 | 3/3 locally accepted |
| `MapCycle100` timing, 3 runs | 8.926416 / 8.926542 / 8.918333 | 2.170500 / 2.258750 / 2.224958 | Aligned RSS: 0.015625 / 0.012500; 0.187500 / 0.112500; 0.046875 / 0.021876 | 3/3 locally accepted |
| `MapCycle100` allocation, 3 admissible runs | 10.060500 / 8.931875 / 8.933125 | 3.847458 / 2.271958 / 2.335458 | Aligned RSS: 0.093750 / 0.031250; 0.203125 / 0.118740; 0.062500 / 0.028125. Aligned live: 0.021275 / 0.000499; 0.095520 / 0.057130; 0.062439 / 0.038062 | 3/3 locally accepted |
| `Soak10Minutes` timing, 1 run | 8.939792 | 2.238250 | Final RSS: 0.046875 / 0.010545 | 1/1 locally accepted |
| `Soak10Minutes` allocation, 1 run | 8.957542 | 2.369417 | Final RSS: 0.031250 / 0.007030; final live: 0.021894 / 0.012469 | 1/1 locally accepted |

Each fixed-combat result proved Bumper Alley index 5, 4 fighters, 4
`Combatant` bots, 4 authored items, and 3 public hazards. Every Map result
proved the 101-asset preload, 10 warm presents, 100 measured switches, 111
total acknowledgements, 11 aligned checkpoints, and stable final resource
counts of 1,528 entities, 222 meshes, 166 materials, and 52 images. All
admissible captures reported
`performance_acceptance_status:"external_gpu_evidence_required"`,
`external_gpu_evidence_status:"required_not_collected"`, and
`gpu_completion_measured:false`. One otherwise locally passing allocation Map
attempt was rejected by the wrapper because AC power changed to battery; the
listed replacement remained on AC throughout.

### Bevy render-pass leak before/after

The pre-backport timing and allocation executable SHA-256 values were,
respectively,
`a70f342091d6543d1b7a05db887ac190d3d59c417e08735656d701ee1b2fab47`
and
`fd6a4ed0de4f9ea43aa4954f24ae840f05df6d37afc84fe230648583f1f76eb8`.
All pre/post comparisons used the same Mac14,6, OS, power policy, resolution,
seed, present mode, compiler profile, and schema-v6 Map fixture.

| Map build | Frame p99 (ms) | CPU p99 (ms) | Aligned RSS range / slope | Aligned live range / slope | Live-byte delta | Result |
| --- | --- | --- | --- | --- | ---: | --- |
| Before timing, 3 runs | 8.990708 / 9.193875 / 8.992834 | 2.193667 / 3.580458 / 2.439333 | 2.421875 / 1.646838; 2.421875 / 1.600076; 2.140625 / 1.424967 | Unavailable in timing build | N/A | 3/3 locally accepted |
| Before allocation, 1 run | 9.267250 | 3.238500 | 2.125000 / 1.402960 | 1.570396 / 1.052623 | 5,752,718 | Rejected: live range and slope |
| After timing, 3 runs | 8.926416 / 8.926542 / 8.918333 | 2.170500 / 2.258750 / 2.224958 | 0.015625 / 0.012500; 0.187500 / 0.112500; 0.046875 / 0.021876 | Unavailable in timing build | N/A | 3/3 locally accepted |
| After allocation, 3 admissible runs | 10.060500 / 8.931875 / 8.933125 | 3.847458 / 2.271958 / 2.335458 | 0.093750 / 0.031250; 0.203125 / 0.118740; 0.062500 / 0.028125 | 0.021275 / -0.000499; 0.095520 / 0.057130; 0.062439 / 0.038062 | 541,262 / 452,848 / 436,808 | 3/3 locally accepted |

Heap attribution found 70,803 retained owned pass-label strings. Bevy 0.18.1
`PassSpanGuard::end` forgot the guard after ending a pass without first dropping
its owned `Cow<'static, str>` name; AFC's one shadowed directional light
generated four dynamic cascade labels per rendered frame. The vendored
backport drops only that owned name before preserving the guard's established
forget behavior. Its exact source provenance, license retention, removal plan,
and patch are recorded in
[vendor/bevy_render/PATCHES.md](../vendor/bevy_render/PATCHES.md). Remove the
override only after an upstream upgrade repeats the allocator Map gate.

### Deterministic-math C1 before/after capture

This comparison measures the v3-preserving C1 change that replaced bounded Bee
and Chick canonical trigonometry with frozen reference tables, replaced authored
arena collision yaw calculations with frozen bases, and prebaked all 91
Champion's Court collision barriers. Both executables used rustc 1.94.1 on the
same Mac14,6, the profiling profile, seed `0x00000000ffc00001`, 256 authority
warmup ticks, 16 rollback warmup bursts, 1,000 timed samples, and exact rollback
depth 12. The machine remained on battery power in High Power mode with no fixed
affinity. Every run passed the executable acceptance gate.

| Build/run | Authority p99 (ns) | Exact 12-tick rollback p99 (ns) | Authority allocations | Rollback allocations/bytes | History/depth |
| --- | ---: | ---: | ---: | ---: | --- |
| Before run 1 | 77,708 | 399,083 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| Before run 2 | 58,292 | 389,625 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| Before run 3 | 62,417 | 390,625 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 run 1 | 62,209 | 375,334 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 run 2 | 59,750 | 378,708 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 run 3 | 66,750 | 391,500 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| Median p99 | 62,209 | 378,708 | 0 | unchanged | Pass / 12 |

The immutable pre-C1 executable SHA-256 is
`6e74f5afffcc90566697046aaf354c738b0405005cd0c5958345de4d511078de`;
the C1 executable SHA-256 is
`c3052221708dba9b757af35932c45ecb30fdccaddea4f9565a83d0e2c7a4aea9`.
The allocation breakdown also remained identical: bot generation, canonical
fixed step, snapshot hashing, and full authority each allocated zero times;
snapshot capture used 26,000 allocations/101,850,000 requested bytes and restore
used 94,008 allocations/40,405,051 requested bytes across 1,000 operations.

### Simulation-v4 canonical-math before/after capture

This comparison measures the simulation-v4 change that pins pure-Rust
`libm 0.2.16` with software floats, centralizes canonical vector length and
normalization, replaces compare-only distances with squared comparisons, and
uses a frozen relative basis for the Chick ultimate. The long rebuild changed
the machine from battery to AC power, so the earlier battery capture was not
used as the v4 comparison. Instead, the immutable C1 and v4 executables were run
interleaved on the same Mac14,6 under AC power, High Power mode, and no fixed
affinity. All other profiler parameters matched the C1 capture exactly.

| Build/run | Authority p99 (ns) | Exact 12-tick rollback p99 (ns) | Authority allocations | Rollback allocations/bytes | History/depth |
| --- | ---: | ---: | ---: | ---: | --- |
| C1 before-v4 run 1 | 93,541 | 359,542 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| v4 run 1 | 81,667 | 363,708 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 before-v4 run 2 | 54,042 | 356,083 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| v4 run 2 | 52,875 | 367,041 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 before-v4 run 3 | 53,833 | 355,291 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| v4 run 3 | 53,292 | 362,417 | 0 | 121,083 / 142,311,564 | Pass / 12 |
| C1 median p99 | 54,042 | 356,083 | 0 | unchanged | Pass / 12 |
| v4 median p99 | 53,292 | 363,708 | 0 | unchanged | Pass / 12 |

The immutable C1 executable SHA-256 is
`c3052221708dba9b757af35932c45ecb30fdccaddea4f9565a83d0e2c7a4aea9`;
the v4 executable SHA-256 is
`1a75f728cbc4a56e65e125080ba65b1f86b6fd288fab2bc42060e591517e57ad`.
Both allocation breakdowns are byte-for-byte equivalent at the reported phase
level. The v4 rollback median increased by 7,625 ns (2.1%) while remaining below
0.364 ms against the 4 ms acceptance budget; authority improved by 750 ns (1.4%).

### Simulation-v5 final gameplay-source capture

This comparison closes the measured-hot-path requirement for the final v5 input
compiler, sequence metadata, completed behavior-fixture tranche, and stock-result
lifecycle publication. The before and after executables were immutable and run
as nine interleaved pairs on the same Mac14,6 under battery power, High Power
mode, and no fixed affinity. Both used rustc 1.94.1, the profiling profile, seed
`0x00000000ffc00001`, 256 authority warmup ticks, 16 rollback warmup bursts,
1,000 timed samples, and exact rollback depth 12. Preliminary scheduling
excursions are retained in the table and in the raw logs rather than discarded;
the median-of-nine summary is robust to them. Every run passed every executable
acceptance gate.

| Pair | Before authority p99 (ns) | After authority p99 (ns) | Before rollback p99 (ns) | After rollback p99 (ns) |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 64,250 | 74,750 | 387,208 | 391,958 |
| 2 | 62,208 | 65,834 | 410,542 | 818,334 |
| 3 | 230,584 | 149,667 | 1,035,084 | 593,375 |
| 4 | 64,417 | 58,458 | 390,125 | 385,959 |
| 5 | 60,750 | 56,375 | 384,000 | 402,125 |
| 6 | 61,750 | 106,750 | 595,250 | 426,667 |
| 7 | 96,333 | 60,042 | 405,291 | 383,959 |
| 8 | 61,333 | 58,125 | 386,833 | 403,083 |
| 9 | 54,125 | 63,583 | 385,916 | 395,834 |
| Median | 62,208 | 63,583 | 390,125 | 402,125 |

Authority p99 changed by +2.2% and exact rollback p99 by +3.1%. Both remain more
than an order of magnitude inside their acceptance budgets. Every before and
after authority run allocated zero times. Every rollback run retained the same
diagnostic 121,083 allocations / 142,311,564 requested bytes, exact depth 12,
and fixed history high-water marks.

The immutable before executable SHA-256 is
`ed7a142226923c9ff1894eb9edd3c284831d9d38ce78abb09357b9ed560ff292`;
the final v5 executable SHA-256 is
`77a14837486e028c6cabd7cc28edcb4ffded9d7af262f5445c52f3ac30f99255`.
The raw result lines are retained under
`target/perf-captures/sequence-metadata/` for the local evidence bundle.

### Canonical SimPosition ownership capture

This comparison closes the measured-hot-path requirement for moving canonical
fighter, hitbox, special, skill, and arena-ordnance pose out of Bevy
`Transform`. The final world now owns gameplay translation in `SimPosition` and
projects it one way into render-only `Transform` state. The immutable final-v5
executable and the canonical-pose executable ran as nine interleaved pairs on
the same Mac14,6 under AC power, High Power mode, and no fixed affinity. Both
used rustc 1.94.1, the profiling profile, seed `0x00000000ffc00001`, 256
authority warmup ticks, 16 rollback warmup bursts, 1,000 timed samples, and an
exact rollback depth of 12. Every power record reported AC power, and every run
passed every executable acceptance gate.

| Pair | Before authority p99 (ns) | After authority p99 (ns) | Before rollback p99 (ns) | After rollback p99 (ns) |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 59,875 | 79,917 | 405,375 | 400,792 |
| 2 | 57,625 | 62,500 | 407,292 | 404,375 |
| 3 | 61,875 | 58,583 | 409,875 | 402,542 |
| 4 | 59,833 | 60,417 | 409,292 | 392,458 |
| 5 | 57,709 | 63,167 | 406,792 | 403,791 |
| 6 | 62,708 | 58,625 | 414,667 | 407,166 |
| 7 | 59,584 | 58,667 | 414,375 | 406,625 |
| 8 | 64,125 | 58,166 | 407,917 | 414,042 |
| 9 | 57,584 | 57,917 | 414,000 | 403,750 |
| Median | 59,833 | 58,667 | 409,292 | 403,791 |

Authority p99 improved by 1.9% and exact rollback p99 improved by 1.3%.
Every authority run remained allocation-free. Removing canonical render
components from the headless world reduced each measured rollback correction by
exactly 1,000 allocations and 32,000 requested bytes, from 121,083 allocations
/ 142,311,564 bytes to 120,083 / 142,279,564. Every run retained exact depth
12, authority history high-water 128, rollback snapshot/input high-water 64/64,
and fixed capacity gates.

The immutable before executable SHA-256 is
`77a14837486e028c6cabd7cc28edcb4ffded9d7af262f5445c52f3ac30f99255`;
the canonical-pose executable SHA-256 is
`d2f59f48bde81fc2ba49514a8f21dca8a115f5d37b19acec10a241d2066fff7f`.
The raw result and power records are retained under
`target/perf-captures/simposition-final/` for the local evidence bundle.

### Superseded v1 FourBotStress evidence

All runs selected Crank Yard (six item anchors, two hazards) and configured four
Bot participants, automatic high-stock continuation, no vsync, the same profiling
executable, and 30 seconds warmup plus 300 seconds measurement. The v1 runner did
not activate `BotBehaviorMode::Combatant`; the reconciled brains remained
`TrainingDummy`, produced neutral input, and therefore did not create the stated
combat workload. Run 3 is retained as the median of that diagnostic workload.

| Run | Frames | Whole frame median/p95/p99/max (ms) | Main-App CPU median/p95/p99/max (ms) | Process CPU median/p95 | RSS median/peak (GiB) |
| --- | ---: | --- | --- | --- | --- |
| 1 | 58,356 | 3.638 / 16.017 / 20.050 / 31.252 | 2.084 / 2.544 / 2.734 / 14.093 | 202.95% / 232.45% | 0.336 / 0.345 |
| 2 | 59,223 | 3.639 / 15.029 / 21.001 / 29.956 | 2.062 / 2.522 / 2.669 / 13.857 | 203.29% / 233.90% | 0.324 / 0.333 |
| 3 (median) | 58,784 | 3.670 / 15.252 / 21.959 / 36.874 | 2.085 / 2.535 / 2.681 / 13.613 | 205.06% / 223.89% | 0.324 / 0.334 |

Median-run allocation/resource observations:

- 365,689,201 allocation/reallocation calls requested 116,699,466,495 bytes.
- Tracked live allocation bytes grew from 105,675,277 to 157,260,096
  (49.20 MiB) and peaked at 157,578,789 bytes.
- Entity and asset counts were constant at 525 entities, 131 meshes, 114
  materials, and 36 images.
- The large allocation volume and repeatable live-byte growth remain useful
  diagnostic signals, but cannot establish a combat baseline or leak without a
  separate allocation build and owner/slope plateau measurements.

### Superseded v1 pre-contact/presentation capture

These three internally valid captures preserve an immediate pre-change diagnostic
for contact arbitration and rollback-safe presentation extraction. They used the
same immutable profiling executable, Apple M2 Max hardware, seeded Crank Yard
workload, no-vsync configuration, 30-second warmup, and 300-second measurement;
the executable was invoked directly with `BEVY_ASSET_ROOT` set to the repository
root. One additional sample was discarded before comparison because foreground
and compositor throttling coincided with external macOS policy load. They share
the v1 idle-dummy/UI/allocator defects and are not a valid combat comparison.

| Run | Frames | Whole frame median/p95/p99/max (ms) | Main-App CPU median/p95/p99/max (ms) | Process CPU median/p95 | RSS median/peak (GiB) | Tracked live-byte growth | Entities/meshes/materials/images |
| --- | ---: | --- | --- | --- | --- | ---: | --- |
| 1 | 42,626 | 7.482458 / 13.551584 / 17.653958 / 35.358084 | 1.812875 / 2.784250 / 3.917958 / 28.200166 | 147.981985% / 161.477944% | 0.361511 / 0.369339 | 49,356,576 bytes | 511 / 131 / 114 / 36 |
| 2 (median whole-frame p95) | 42,111 | 7.480958 / 13.576709 / 18.074083 / 55.769875 | 1.933583 / 3.227709 / 4.536250 / 55.409583 | 150.167555% / 169.666484% | 0.355728 / 0.364059 | 49,258,683 bytes | 511 / 131 / 114 / 36 |
| 3 (valid) | 55,673 | 3.865625 / 15.917667 / 21.715959 / 29.802875 | 2.130125 / 3.016000 / 4.142750 / 15.451125 | 199.872117% / 239.896731% | 0.358887 / 0.367676 | 51,235,082 bytes | 511 / 131 / 114 / 36 |

Across the three valid samples, the median whole-frame p95 is 13.577 ms (run 2)
and the independently selected median main-App CPU p95 is 3.016 ms. These values
must not be used to accept a schema-v6 baseline.

### Superseded v1 MapCycle100 evidence

All runs used the same no-vsync profiling executable and seed, a 30-second warmup,
and 300 seconds of measurement. The v1 counter reported 100 switches, but advanced
in the next `Last` schedule without waiting for scene instances or a rendered
frame. Run 3 is retained as the median diagnostic by whole-frame p95; run 2 is the
median by main-App CPU p95.

| Run | Frames | Whole frame median/p95/p99/max (ms) | Main-App CPU median/p95/p99/max (ms) | Switch median/p95/p99/max (ms) | RSS start/end/peak (GiB) |
| --- | ---: | --- | --- | --- | --- |
| 1 | 58,009 | 3.845 / 14.912 / 20.433 / 170.269 | 1.789 / 2.441 / 2.647 / 7.833 | 3.483 / 5.308 / 5.476 / 5.476 | 0.284 / 0.370 / 0.399 |
| 2 | 58,299 | 3.792 / 15.066 / 20.719 / 29.812 | 1.900 / 2.502 / 2.733 / 14.372 | 3.495 / 5.365 / 5.766 / 5.766 | 0.281 / 0.351 / 0.397 |
| 3 (median whole-frame p95) | 57,002 | 3.940 / 15.064 / 20.395 / 34.897 | 1.901 / 2.551 / 3.058 / 13.620 | 3.435 / 5.213 / 5.496 / 5.496 | 0.279 / 0.418 / 0.418 |

All three runs peaked and ended at the same resource counts: 1,448/826 entities,
166/128 meshes, 142/112 materials, and 39/37 images. Tracked live bytes grew by
11.1-11.7 MiB per run; run 3 grew from 111,648,173 to 123,821,414 bytes and peaked
at 232,296,377 bytes. The process-RSS ending value varied more than tracked live
allocations, so both measurements remain in future comparisons.

Every return to Crank Yard emitted Bevy hierarchy warning `B0004` for
`lever.colormap`. Subsequent investigation identifies this as Bevy's
insertion-order false positive tracked in
[bevyengine/bevy#19776](https://github.com/bevyengine/bevy/issues/19776), not
evidence that the child outlived or lost its arena parent. Retain the warning in
raw logs as an upstream diagnostic; schema-v6 exact resource checkpoints and
zero stale-owner counts remain the acceptance evidence.

### Superseded v1 Soak10Minutes evidence

The seeded Crank Yard workload configured four Bot participants, six item anchors,
two hazards, no vsync, the same profiling executable, a 30-second warmup, and 600
seconds of measurement. Those brains remained idle `TrainingDummy` instances, so
this is not a combat soak.

| Frames | Whole frame median/p95/p99/max (ms) | Main-App CPU median/p95/p99/max (ms) | First/last minute frame p95 (ms) | First/last minute CPU p95 (ms) | RSS start/end/peak (GiB) |
| ---: | --- | --- | --- | --- | --- |
| 113,779 | 3.720 / 15.886 / 21.170 / 32.286 | 2.126 / 2.611 / 2.987 / 24.518 | 14.980 / 16.006 | 2.608 / 2.642 | 0.289 / 0.354 / 0.354 |

Entity and asset counts remained exactly stable at 525 entities, 131 meshes, 114
materials, and 36 images. The counting allocator recorded 707,587,412 allocation
or reallocation calls requesting 225,637,548,917 bytes. Tracked live bytes grew
from 108,341,801 to 167,966,363 bytes (56.86 MiB) and peaked at 168,285,702 bytes.

The CPU p95 drift was only 0.034 ms (about 1.3%), while whole-frame p95 increased
1.027 ms (about 6.9%). Stable ECS/asset counts rule out those registries as the
source of growth. The final-start delta alone does not distinguish a leak from a
late cache plateau, especially without owner counters. Treat it as a measurable
pre-existing cache-growth signal that the schema-v6 bounded owner/slope soak
must diagnose.

## Measurement rules

1. Warm up the scenario for 30 seconds, then sample for at least five minutes.
2. Run each short scenario three times for the timing build and three times for
   the separate allocation build; report the median timing run plus its
   p95/p99 and retain every JSON record. Run one complete ten-minute soak per
   build. This matrix has 87 minutes of warmup/measurement time before
   readiness and final-fence overhead.
3. Change one optimization category at a time and keep the unmodified measurement.
4. Reject a result that changes replay state, gameplay timing, visible high-quality
   output, or test behavior even if it is faster.
5. Do not gate normal CI on frame timing; shared runners are noisy. Gate tests,
   supported feature builds, and WASM size, while keeping hardware-stamped timing
   reports in this table.
6. Update the accepted baseline only after the improvement is reproduced. Change
   `WASM_SIZE_BUDGET_BYTES` only with a documented reason and a new measured result.

Add a spatial broadphase only if cached arena queries still exceed 10% of CPU frame
time. Centralize impact requests only if combat remains above 15%. Pool complex GLTF
scenes only if scene creation remains above 5%. These thresholds prevent architecture
cost from being added without a measured return.
