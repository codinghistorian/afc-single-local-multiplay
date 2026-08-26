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
| itch.io archive | `./scripts/package_itch.sh` | Validates and writes `target/itch/animal-fighter-club-web.zip`. |

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
complete itch.io-ready static artifact to `web_dist/`. The tracked `web/index.html`
template keeps `pkg/` and `assets/` references relative and sizes the Bevy canvas
to its containing iframe. Run `./scripts/package_itch.sh` to verify itch.io's
HTML5 limits and create a root-layout ZIP without a wrapping `web_dist/` folder.
Before publishing, upload the ZIP as a draft HTML Game with click-to-launch
fullscreen enabled, then verify controller discovery, audio startup, reconnect,
and replacement-controller behavior in current Chrome and Safari with real
hardware.

## DualSense physical QA

The standard DualSense is the required PlayStation hardware target. DualSense
Edge should retain normalized compatibility, but its extra controls are not bound
or included in acceptance. On Linux, use a current kernel with `hid-playstation`
and confirm that the user has input and force-feedback permissions.

Run this matrix with both USB and Bluetooth transports:

| Runtime | Operating system | USB | Bluetooth |
| --- | --- | --- | --- |
| Native | Windows 10/11 | Required | Required |
| Native | Current Linux | Required | Required |
| Native | Current macOS | Required | Required |
| Current Chrome | Windows 10/11 | Required | Required |
| Current Chrome | Current Linux | Required | Required |
| Current Chrome | Current macOS | Required | Required |
| Current Safari | Current macOS | Required | Required |

For every row and transport:

1. Connect before launch, connect after launch, disconnect, and reconnect.
2. Verify left stick and D-pad movement; Cross join/confirm/jump; Circle
   leave/back/grab; Square light; Triangle heavy; L2 aim; R2 guard; R1 dash; L1
   ultimate; and Options menu/pause behavior. Confirm the stick moves once per
   neutral deflection and the D-pad repeats only after its initial delay.
3. Verify controller setup, single-player two-press takeover and cancellation,
   multiplayer ownership, menus, gameplay, and combat disconnect pausing.
4. Reclaim a missing seat with the original controller and with an unassigned
   replacement. Repeat with mixed Xbox and DualSense ownership.
5. Verify family-specific tutorial, arena, takeover, and reconnect prompts.
6. Verify vibration for every assigned controller, including both roles of a
   mixed DualSense/Xbox match and simultaneous combat feedback. On macOS, confirm
   the Xbox route reports `default actuator`. Where an operating system or browser
   exposes no usable actuator, accurately reported unsupported capability passes;
   loss of input or reconnect behavior does not.

Before accepting native changes, run `cargo run` and `cargo test` on Windows,
Linux, and macOS. When a browser build is explicitly requested, run
`./scripts/build_web.sh` and keep the artifact in repository-root `web_dist/`; do
not create an itch.io archive unless it is separately requested.
