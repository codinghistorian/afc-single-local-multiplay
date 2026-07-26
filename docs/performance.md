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
| 2026-07-26 | Apple M2 Max, macOS | itch.io web release with Bevy Gilrs | Optimized WASM 12,515,927 bytes; `web_dist/` 64,722,014 extracted bytes across 207 files; ZIP 48,525,710 bytes. | Size/package baseline; draft itch.io Chrome and Safari runtime measurements remain pending. |
| Pending | Apple M2 Max, macOS | `FourBotStress`, profiling profile | Capture median, p95, p99, CPU/GPU time, allocations, entities, assets, and memory. | Required before accepting hot-path gains. |
| Pending | Apple M2 Max, macOS | `MapCycle100` and `Soak10Minutes` | Capture peak/end counts and memory. | Required before accepting cache or pool changes. |
| Pending | Current Chrome and Safari | Optimized web release | Capture p95/p99 frame time, WASM, and distribution size. | Required before changing the accepted web baseline. |

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
