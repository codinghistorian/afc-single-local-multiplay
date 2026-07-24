# Steam Input Integration

Status: implemented in the native `steam-net` build. Real-device and Steam Deck
release acceptance still require the hardware checks at the end of this document.

## Runtime contract

`SteamPlatform<RealSteamBackend>` remains the only owner of the Steam client and
callback pump. Its callback step also calls `ISteamInput::RunFrame`; no second
Steam client or callback owner is introduced.

Overlay readiness is queried at the moment an invite or binding surface is
requested. Overlay activity is the latest coalesced
`GameOverlayActivated_t` value, not a queued gameplay event. An active overlay
never pauses an online match or its callback/network pumps; it temporarily
causes locally owned seats to submit neutral gameplay input.

The backend resolves action handles once during startup and never exposes them.
Every render frame it publishes one fixed-size `SteamInputSnapshot` containing at
most four stable local controller ordinals. Each controller contains only:

- a process-local controller ID used to preserve its couch ordinal;
- a presentation-only device kind;
- quantized `Move` axes;
- the current held mask for the eight existing gameplay actions; and
- the current held mask for menu accept, back, navigation, and binding-panel
  actions.

Only `QuantizedMovement` and `InputMask` values enter `LocalTickInputState`.
Controller IDs, device kinds, Steam action handles, and menu actions never enter
the gameplay protocol, replay input frames, snapshots, or deterministic state.

Connected handles are reconciled without allocation. A controller keeps its
ordinal when Steam changes callback/list order. A disconnected ordinal is made
available for a newly connected controller; surviving controllers are not
compacted into different player seats.

## Action sets and defaults

The bundled [action manifest](../assets/steam_input/action_manifest.vdf) has two
action sets. Separating them prevents a face button from firing `Jump` and menu
accept at the same time.

| Set | Analog actions | Digital actions |
| --- | --- | --- |
| `Gameplay` | `Move` | `Left`, `Right`, `Up`, `Down`, `AimGrab`, `Heavy`, `Light`, `Jump`, `MenuBack`, `MenuBindings` |
| `Menu` | none | `MenuAccept`, `MenuBack`, `MenuUp`, `MenuDown`, `MenuLeft`, `MenuRight`, `MenuBindings` |

The application selects `Gameplay` only during an offline fight or an online
worker's fighting/result-confirmation phase. All setup, lobby, loading,
reconnect, results, and error screens use `Menu`.

The bundled configurations provide a Steam Deck layout and a generic gamepad
layout. The generic configuration is selected for Xbox, PlayStation, Switch Pro,
paired Joy-Con, mobile-touch, and generic controller categories so Steam can
apply its device remapping.

Gameplay defaults:

- left stick: move;
- D-pad: directional move/dash actions;
- A / Cross: jump;
- B / Circle: aim/grab;
- X / Square: light attack;
- Y / Triangle: heavy attack;
- menu/start: back/leave action; and
- view/select: open the Steam controller-layout panel.

Menu defaults:

- D-pad: move focus;
- A / Cross: accept;
- B / Circle or menu/start: back; and
- X / Square or view/select: open the Steam controller-layout panel.

Menu focus skips unavailable actions, wraps in both directions, and is rendered
with a distinct border. Keyboard and pointer commands remain available and take
dispatch priority if several devices act in one render frame.

Native release builds cold-boot into the player-facing title screen; debug builds
retain the existing developer sandbox entry. Steam menu actions are independently
edge-latched across the title, mode-select, and online-screen boundaries, so the
button that opens one screen cannot also accept an action on the next. From a
controller-only launch, A/Cross opens mode select, the D-pad moves the highlighted
choice, A/Cross selects Online, and B/Circle returns from mode select to the title.
Offline menu choices, pointer input, keyboard input, and the debug Shift+U entry
remain available. The controller-layout action opens Steam's binding panel for
the exact connected local controller that requested it. Target validation runs
before overlay readiness. A disabled/unavailable panel returns a typed local
status and shows one sanitized, explicitly dismissible four-second notice; it
does not enter Error, switch action sets, or stop a worker.

## Input latching and keyboard coexistence

Keyboard and Steam controller values occupy separate source channels inside each
seat accumulator. Continuous movement is combined and normalized back into the
unit circle, and held actions are unioned. A release from one device does not release an action
still held by the other device.

Steam exposes current action state rather than Bevy-style edges. The accumulator
derives per-source transitions and union-latches them until the next fixed tick,
using the same `LocalTickInputState` drain as keyboard input. This preserves taps
when a render frame has no fixed step and exposes edges only to the first fixed
step during catch-up.

The Steam-enabled shipping build samples controllers in offline fights as well as
online fights. Builds without `steam-net`, including web builds, return an empty
snapshot and do not compile or link `steamworks`; their keyboard path is unchanged.

## Startup and fail-closed asset loading

Steam Input startup fails the native online runtime if any of these checks fail:

1. `ISteamInput::Init(true)` succeeds;
2. the manifest is a bounded UTF-8 file with balanced VDF strings/braces;
3. all required action-set and action names are present;
4. both referenced configuration files exist and contain the required presets
   and bindings;
5. `SetInputActionManifestFilePath` accepts the absolute path; and
6. every required Steam action handle is non-zero.

The runtime searches these locations in order:

1. `AFC_STEAM_INPUT_MANIFEST`, when explicitly set;
2. `<executable>/steam_input/action_manifest.vdf`;
3. `<executable>/assets/steam_input/action_manifest.vdf`;
4. macOS `<App>.app/Contents/Resources/steam_input/action_manifest.vdf`; and
5. the repository asset path for `cargo run` development.

The sealed candidate uses `<executable>/assets/steam_input/` on Windows, Linux,
and inside the macOS application's `Contents/MacOS` directory, so it resolves
through item 3. Do not assemble that layout by hand. `scripts/release.py stage`
copies the tracked runtime-asset set, and both `stage` and `verify` require the
manifest plus the generic-gamepad and Steam Deck configurations. The release
policy excludes only data compiled into the executable.

In the Steamworks Partner site, set Steam Input's default configuration to
**Custom Configuration (Bundled with game)** and point it at the depot copy of
`action_manifest.vdf`. This external setting and its physical-device result must
be recorded against the exact archive hash and release identity; preview-only
SteamPipe VDF generation does not configure it. See
[Native release packaging](release-packaging.md). Valve documents the action
manifest/configuration flow in the
[Action Manifest Files guide](https://partner.steamgames.com/doc/features/steam_controller/action_manifest_file?language=english)
and the action schema in the
[In-Game Actions File guide](https://partner.steamgames.com/doc/features/steam_controller/iga_file?language=english).

## Automated coverage

Focused tests cover:

- stable ordinals under reordered connected-handle lists and disconnect/rejoin;
- deterministic fake-backend action injection and binding-panel routing;
- disabled/enabled overlay ordering, exact controller targeting, coalesced
  activity, notice expiry/dismissal, and continued neutral tick drain;
- bundled manifest/config validation plus malformed-VDF rejection;
- keyboard/controller held-state coexistence and edge latching;
- controller menu focus, accept, back, and layout-panel intents; and
- controller-only title-to-Online navigation with held-button transition guards;
- analog/D-pad camera-relative gameplay mapping.

Required commands for this slice:

```text
cargo check --lib
cargo check --lib --locked --no-default-features --features native,steam-net
cargo test tick_input::tests --lib
cargo test steam_platform::tests::steam_input --lib
cargo test native_online_app::tests::controller --lib
cargo test user_mode::tests::steam_controller --lib
cargo test --lib --locked --no-default-features \
  --features native,steam-net \
  bundled_production_steam_input_manifest_and_configurations_validate
```

## Release hardware acceptance

These checks need Steam accounts and physical hardware and therefore cannot be
substituted by CI:

- Launch the depot build on Steam Deck using only built-in controls; cross the
  title and mode-select screens, create/join a lobby, edit a couch seat, ready,
  fight, rematch, return to mode select, and return to the title.
- Repeat on Windows with Xbox and PlayStation controllers and on Linux with a
  generic controller.
- Connect four controllers, verify stable P1-P4 assignment while Steam changes
  enumeration order, then disconnect/reconnect each controller.
- Rebind every gameplay and menu action in Steam's layout panel and verify the
  protocol/replay input frame remains unchanged in shape.
- Confirm glyph/layout overlay behavior with the Steam overlay both enabled and
  intentionally disabled; a disabled overlay must return locally without
  stalling input or the callback pump.

Record device models, OS/Steam client versions, sealed archive SHA-256, release
identity, Steam-assigned depot build ID, and pass/fail evidence in the
[release checklist](steam-release-acceptance.md). These are release-evidence
gates, not missing runtime implementation, and no current automated result
substitutes for them.
