# Development Workflow

This project treats performance as a tested behavior, not as a cleanup phase. New
features should preserve deterministic gameplay, use the established subsystem
boundaries, and include measurements when they change a hot path.

## Commands and profiles

| Purpose | Command | Notes |
| --- | --- | --- |
| Normal development | `cargo run` | App code uses optimization level 1; dependencies use level 3. |
| Required tests | `cargo test` | Run after every code change. |
| Release runtime | `cargo run --release` | Fat LTO, one codegen unit, stripped symbols, abort-on-panic. |
| Instrumented runtime | `cargo run --profile profiling --features perf` | Release optimization with debug symbols. |
| Tracy capture | `cargo run --profile profiling --features trace` | Native-only Tracy instrumentation. |
| Web distribution | `./scripts/build_web.sh` | Writes only to repository-root `web_dist/`. |

The `perf` feature is the gate for low-overhead counters and benchmark scenarios.
The `trace` feature includes `perf` and Bevy's Tracy integration. Neither belongs
in shipping builds.

## Change workflow

1. Record correctness and performance baselines before changing a measured path.
2. Make the smallest architectural change that removes the measured cost.
3. Run `cargo test` and launch `cargo run` after every code change.
4. Run the relevant scenario from `performance.md` for scheduling, collision,
   combat, asset, rendering, UI, or allocation changes.
5. Build the web distribution when changing dependencies, features, profiles,
   assets, shaders, or web code.
6. Update the documentation when an ownership boundary, invariant, command, or
   accepted performance baseline changes.

## Rules for new systems

- Put deterministic gameplay in the fixed simulation schedule. Keep input
  sampling, visual interpolation, cameras, and UI presentation in the frame
  schedule.
- Express ordering with system sets and explicit dependencies. Do not chain
  unrelated systems merely to silence an ECS access conflict.
- Cache immutable definitions, asset handles, meshes, materials, arena collision
  data, and stable entity relationships in an owning resource or component.
- Use events, state conditions, and Bevy change detection to avoid scanning or
  writing unchanged data.
- Do not load assets, create meshes or materials, format HUD strings, or allocate
  growable scratch collections every steady-state frame.
- Bound transient effects. Reuse high-frequency entities and clear their state
  deterministically when returning them to a pool.
- Preserve stable fighter and impact ordering. Performance work must not change
  hit priority, random consumption, or replay results without an explicit design
  decision.

## Dependency and feature policy

Bevy currently retains its default features. The game uses a broad combination of
PBR, GLTF scenes, UI, audio, windows, input, gizmos, and native file watching, and
the repository does not yet have a verified minimal native/web feature matrix.
Feature pruning is therefore deferred until it can be audited independently.

Before disabling defaults, inventory active features with `cargo tree -e features`,
compile native, test, profiling, and web configurations, then play every arena and
fighter. Keep only a feature set that passes that matrix. Never ship Bevy dynamic
linking; it is suitable only for an explicitly local iteration configuration.

## Web prerequisites

Install the `wasm32-unknown-unknown` target, matching `wasm-bindgen-cli`, and
Binaryen's `wasm-opt`. The build script runs `wasm-opt -O3`, reports raw and final
WASM sizes, enforces the documented guardrail, copies static assets, and writes the
complete GitHub Pages artifact to `web_dist/`.
