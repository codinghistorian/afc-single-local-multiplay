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

| Scenario | Workload | Required observations |
| --- | --- | --- |
| `FourBotStress` | Four seeded fighters, maximum normal items, hazards, projectiles, and effects for five minutes. | Median, p95, p99 CPU/GPU frame time, allocations, entity and asset counts. |
| `MapCycle100` | Cycle through every arena ten times with normal setup and teardown. | Peak and ending entities, meshes, materials, memory, and switch latency. |
| `Soak10Minutes` | Ten minutes of uninterrupted seeded combat. | Frame-time drift, peak memory, pool growth, and stale entities. |

Run with vsync disabled for frame-cost measurements so the limiter does not hide
work. Keep normal gameplay on automatic vsync. Use a release-equivalent profile:

```bash
cargo run --profile profiling --features perf
```

For a Tracy capture:

```bash
cargo run --profile profiling --features trace
```

Web measurements use a local `./scripts/build_web.sh` artifact and current Chrome
and Safari. Record the optimized WASM and total distribution sizes printed by the
script.

## Baseline record

| Date | Hardware and platform | Scenario/build | Result | Status |
| --- | --- | --- | --- | --- |
| 2026-07-22 | Apple M2 Max, macOS | Existing generated WASM | Approximately 78.2 MiB before the optimization program | Planning baseline; replace with exact scripted result. |
| 2026-07-26 | Apple M2 Max, macOS | itch.io web release with Bevy Gilrs | Optimized WASM 12,515,927 bytes; `web_dist/` 64,722,014 extracted bytes across 207 files; ZIP 48,525,710 bytes. | Invalid packaging measurement: the build script selected a stale headless dedicated-server WASM; superseded below. |
| 2026-07-27 | Apple M2 Max, macOS | Corrected itch.io web release | Optimized game WASM 44,263,596 bytes; `web_dist/` 103,509,569 extracted bytes across 214 files; ZIP 64,575,622 bytes. | Corrected size/package baseline after pinning the `ffc-prototype` browser-game binary; Chrome and Safari runtime measurements remain pending. |
| Pending | Apple M2 Max, macOS | `FourBotStress`, profiling profile | Capture median, p95, p99, CPU/GPU time, allocations, entities, assets, and memory. | Required before accepting hot-path gains. |
| Pending | Apple M2 Max, macOS | `MapCycle100` and `Soak10Minutes` | Capture peak/end counts and memory. | Required before accepting cache or pool changes. |
| Pending | Current Chrome and Safari | Optimized web release | Capture p95/p99 frame time, WASM, and distribution size. | Required before changing the accepted web baseline. |

## Hot-path validation record

The Split Causeway interactive-gate change was checked with three paired
`FourBotStress` runs on 2026-08-02. The before build was commit `c13dcd8`; the
after build was the interactive-gate working tree. All six runs used the same
Apple M2 Max with 12 logical CPUs, macOS 26.5.1, Metal, the profiling profile
with `perf`, a 1280x720 window, the `FFC00001` seed, Split Causeway, four normal
items, four arena hazards, and four seeded bots. Every run warmed up for 30
seconds and sampled for 300 seconds. The reported rows are the middle runs when
ordered by frame median.

| Build | Samples | Frame median / p95 / p99 | Render CPU span median / p95 / p99 | Process CPU | RSS peak / end | Entities peak / end | Mesh allocations peak / end | Assets: meshes / materials / images / scenes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Before median: `c13dcd8`, run 1 of 3 | 18,358 | 16.6388 / 17.5997 / 18.3228 ms | 0.0915 / 0.1349 / 0.1605 ms | 86.18% total; 7.18% normalized | 0.4599 / 0.4597 GiB | 1,064 / 1,026 | 126 / 126 | 126 / 130 / 54 / 40 |
| After median: interactive gates, run 2 of 3 | 30,010 | 8.6318 / 17.0642 / 18.7315 ms | 0.0700 / 0.1095 / 0.1411 ms | 111.54% total; 9.30% normalized | 0.4454 / 0.4451 GiB | 1,177 / 1,036 | 126 / 126 | 126 / 130 / 54 / 40 |

Frame median changed by -48.12%, p95 by -3.04%, and p99 by +2.23%. Render
CPU-span median changed by -23.50%, p95 by -18.83%, and p99 by -12.09%. These
numbers mix different presentation regimes and are not evidence that the gate
feature improved performance: all three before medians were paced near 60 Hz
(16.6080, 16.6388, and 16.6520 ms), while all three after medians were mostly
unlocked (8.4945, 8.6318, and 8.8116 ms). That also explains the higher process
CPU while the after build rendered substantially more frames. `AutoNoVsync`
was requested for every run, but macOS presentation did not behave consistently
between the paired sets, and Bevy's Metal backend did not expose GPU timestamps.

The after median ended with ten more entities, while mesh-allocation and all
asset counts were unchanged and RSS did not grow through the sample. The gate
work is bounded to two dynamic barriers and four eligible fighters, and prompt
text/layout changes are cached. This is a functional hot-path validation record,
not an optimization claim; it does not replace an accepted baseline row or
change any target.

The shared-special runtime-gate change was checked with three paired
`FourBotStress` runs on 2026-07-28. The before build was commit `782366d`; the
after build was the shared-special working tree. All six official runs used the
same Apple M2 Max with 12 logical CPUs, macOS 26.5.1, Metal, the profiling
profile with `perf`, a 1280x720 window, the `FFC00001` seed, Crank Yard, six
normal items, two arena hazards, and four Catalyst bots. Every run warmed up for
30 seconds and sampled for 300 seconds. The reported rows are the middle runs
when ordered by frame median, as required by the three-run protocol.

| Build | Samples | Frame median / p95 / p99 | Render CPU span median / p95 / p99 | Process CPU | RSS peak / end | Entities peak / end | Mesh allocations peak / end | Assets: meshes / materials / images / scenes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Before median: `782366d`, run 2 of 3 | 19,377 | 16.5813 / 17.6813 / 18.1715 ms | 0.0760 / 0.1211 / 0.1390 ms | 96.29% total; 8.02% normalized | 0.4369 / 0.4369 GiB | 765 / 761 | 127 / 127 | 127 / 117 / 46 / 44 |
| After median: shared-special gate, run 2 of 3 | 27,310 | 13.3743 / 19.2382 / 20.8904 ms | 0.0713 / 0.1312 / 0.1676 ms | 119.98% total; 10.00% normalized | 0.4292 / 0.4128 GiB | 754 / 749 | 127 / 127 | 127 / 117 / 46 / 44 |
| After investigation: extra fully paced run | 18,004 | 16.6604 / 17.8191 / 19.2778 ms | 0.0826 / 0.1507 / 0.1872 ms | 92.16% total; 7.68% normalized | 0.4067 / 0.4067 GiB | 754 / 749 | 127 / 127 | 127 / 117 / 46 / 44 |

The official median's frame median changed by -19.34%, p95 by +8.81%, and p99
by +14.96%. Those values mix different presentation regimes and must not be
read as either a workload speedup or an accepted regression. `AutoNoVsync` was
requested for every run, but rolling FPS diagnostics show that the before
median was paced below 70 FPS for 144 of 150 sample snapshots, while the after
median switched repeatedly between paced and unlocked presentation and was
below 70 FPS for only 109 of 150 snapshots. The three before frame medians were
4.1691, 16.5813, and 16.6717 ms; the three after medians were 8.4164, 13.3743,
and 16.6863 ms.

The fully paced after run 3 and one extra investigation run both stayed below
70 FPS for all 150 diagnostic snapshots. The extra run was +0.48% at frame
median and +0.78% at p95 relative to the before median, but its p99 was +6.09%
(+1.1063 ms). The official after run 3 reproduced that tail at 19.2816 ms, so
the p99 threshold observation is retained rather than dismissed. The harness
cannot separate replacement-action workload from macOS presentation jitter,
and Bevy's Metal backend did not expose GPU timestamps.

The investigation found no non-special attack tuning change: the source diff
does not touch combat, techniques, items, or any character-skill module, bots
fall through to the established movement and attack selection paths, and the
full 725-test suite remains green. Peak/end entities fell by 11/12 in the
official median, asset and mesh-allocation counts stayed flat, and RSS did not
grow through the sample. This is a functional workload change because bots no
longer spawn shared specials; it is not an optimization claim, does not replace
an accepted baseline row, and does not change any target.

The guided-tutorial change was checked with one paired `FourBotStress` run on
2026-07-27. Both builds used an Apple M2 Max with 12 logical CPUs, macOS 26.5.1,
the profiling profile with `perf`, a 1280x720 window, the `FFC00001` seed, Crank
Yard, six normal items, two arena hazards, and four Catalyst bots. Each run
warmed up for 30 seconds and sampled for 300 seconds with its matching asset
directory present.

| Build | Frame median / p95 / p99 | Render CPU span median / p95 / p99 | Process CPU | RSS peak / end | Entities peak / end | Mesh allocations peak / end | Assets: meshes / materials / images / scenes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Before: `d7008e2` plus the measurement harness only | 16.6828 / 18.2022 / 18.8242 ms | 0.0882 / 0.1388 / 0.1589 ms | 74.96% total; 6.25% normalized | 0.3208 / 0.3208 GiB | 592 / 589 | 125 / 125 | 125 / 113 / 37 / 44 |
| After: guided tutorial working tree | 16.6831 / 18.2011 / 18.8376 ms | 0.0855 / 0.1349 / 0.1552 ms | 74.51% total; 6.21% normalized | 0.3279 / 0.3279 GiB | 698 / 698 | 125 / 125 | 125 / 113 / 38 / 44 |

Frame median changed by +0.002%, p95 by -0.006%, and p99 by +0.071%;
process CPU changed by -0.45 percentage points. The added tutorial UI accounts
for 106 peak entities and one image, while meshes, materials, scenes, and mesh
allocations are unchanged. Peak and ending RSS are equal within each run, and
the after-run entity count does not grow during the sample. Bevy's Metal backend
did not expose GPU timestamps, so GPU time is unavailable. `AutoNoVsync` was
requested by the `perf` feature, but observed presentation remained paced near
60 Hz on this configuration.

This is a functional-change regression record, not a reproduced optimization
gain, so it does not replace the accepted baseline rows or change any target.

The manual-aim and floating-crosshair change was checked with one paired
`FourBotStress` run on 2026-07-27. Both builds used the same Apple M2 Max,
macOS 26.5.1, profiling profile with `perf`, 1280x720 window, `FFC00001` seed,
Crank Yard, six normal items, two arena hazards, and four Catalyst bots. Each
valid run warmed up for 30 seconds and sampled for 300 seconds. The before run
used commit `3ae0a97`; the after run used the manual-aim working tree.

| Build | Frame median / p95 / p99 | Render CPU span median / p95 / p99 | Process CPU | RSS peak / end | Entities peak / end | Mesh allocations peak / end | Assets: meshes / materials / images / scenes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Before: `3ae0a97` | 16.6110 / 19.8663 / 21.1332 ms | 0.0802 / 0.1356 / 0.1748 ms | 82.66% total; 6.89% normalized | 0.3337 / 0.3337 GiB | 714 / 710 | 125 / 125 | 125 / 113 / 38 / 44 |
| After: manual aim working tree | 16.6464 / 17.7631 / 19.7412 ms | 0.0826 / 0.1326 / 0.1654 ms | 89.61% total; 7.47% normalized | 0.3439 / 0.3439 GiB | 734 / 734 | 127 / 127 | 127 / 117 / 38 / 44 |

Frame median changed by +0.21%, p95 by -10.59%, and p99 by -6.59%. Render
CPU-span median changed by +2.99%, p95 by -2.21%, and p99 by -5.38%; total
process CPU changed by +6.95 percentage points. The persistent procedural
crosshairs account for 24 fixed entities, two shared meshes, and four
player-colored materials. Counts and RSS were flat within the after sample, so
there is no evidence of per-frame aim allocation or entity growth. Metal did
not expose GPU timestamps.

This is a functional-change regression record, not an accepted performance
baseline change. The paired samples show no material frame-time regression, so
the baseline rows and targets remain unchanged.

## Measurement rules

1. Warm up the scenario for 30 seconds, then sample for at least five minutes.
2. Run each short scenario three times and report the median run plus its p95/p99.
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
