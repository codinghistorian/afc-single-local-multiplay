# Development Workflow

This project treats performance as a tested behavior, not as a cleanup phase. New
features should preserve deterministic gameplay, use the established subsystem
boundaries, and include measurements when they change a hot path.

## Commands and profiles

| Purpose | Command | Notes |
| --- | --- | --- |
| Normal development | `cargo run` | App code uses optimization level 1; dependencies use level 3. |
| Required tests | `cargo test` | Run after every code change. |
| Optimized local runtime | `cargo run --release` | Release profile, but not a Steam candidate: default development features remain enabled. |
| Steam-enabled development | `cargo test --locked --no-default-features --features native,steam-net` | Player-shaped Steam compile/test contract without release identity or packaging. |
| Instrumented runtime | `cargo run --profile profiling --features perf` | Release optimization with debug symbols. |
| Tracy capture | `cargo run --profile profiling --features trace` | Native-only Tracy instrumentation. |
| Web distribution | `./scripts/build_web.sh` | Writes only to repository-root `web_dist/`. |

The `perf` feature is the gate for low-overhead counters and benchmark scenarios.
The `trace` feature includes `perf` and Bevy's Tracy integration. Neither belongs
in shipping builds.

## Native Steam release candidates

A shipping executable uses one exact feature composition and immutable build
inputs:

```bash
AFC_BUILD_ID=<IMMUTABLE_RELEASE_LABEL> \
AFC_STEAM_APP_ID=<REAL_AFC_APP_ID> \
  cargo build --locked --release --no-default-features --features shipping \
  --bin ffc-prototype
```

The raw executable is not a candidate. Start with:

```bash
python3 scripts/release.py self-test -v
python3 scripts/release.py audit-source
```

Then use `scripts/release.py stage`, `verify`, and `archive` with the
platform-specific arguments in [Native release packaging](release-packaging.md).
Those commands require a clean committed source tree, query
`--release-identity` before sealing, copy the tracked runtime assets and matching
Steam API library, and produce a deterministic manifest/checksum contract.

The protected
[native release-candidate workflow](../.github/workflows/release-candidate.yml)
builds all three native depots from one commit. Linux is built inside the exact
Steam Linux Runtime 4 SDK image pinned by tag and digest in
`packaging/release-policy.json`; App ID 4183110 is a separate Steamworks Partner
runtime selection, not the game's compiled App ID. The workflow produces
unsigned internal archives and preview-only SteamPipe VDFs. It does not perform
production signing/notarization, upload to Steam, promote a branch, or replace
the physical two-account/device acceptance record. The repository supplies no
product IDs; a protected environment must provide the approved App/depot values.

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

Bevy dependency defaults are disabled and the repository selects its required
rendering, asset, audio, UI, and input capabilities explicitly. Project features
then choose the platform composition: `native`, `web`, `steam-net`, `perf`, and
`trace`. The ordinary default adds `dev-hot-reload`; the `shipping` feature adds
exactly `native` plus `steam-net` and must be selected with
`--no-default-features` so developer file watching cannot enter a candidate.

Before changing this matrix, inventory active features with
`cargo tree -e features`, compile native, Steam, profiling, and web
configurations, then play every arena and fighter. Keep only a feature set that
passes that matrix. Never ship Bevy dynamic linking; it is suitable only for an
explicitly local iteration configuration.

## Web prerequisites

Install the `wasm32-unknown-unknown` target, matching `wasm-bindgen-cli`, and
Binaryen's `wasm-opt`. The build script runs `wasm-opt -O3`, reports raw and final
WASM sizes, enforces the documented guardrail, copies static assets, and writes the
complete GitHub Pages artifact to `web_dist/`.
