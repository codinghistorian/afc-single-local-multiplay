# FFC Prototype

Runnable Rust/Bevy prototype for an original low-poly 3D arcade arena brawler. It combines custom Bevy geometry with a curated set of CC0 Kenney assets from `arts/`, without a third-party physics crate.

## Run

```bash
cargo run
```

The window opens at 1280x720.

## Web build

One-time setup:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.121
brew install binaryen
```

`binaryen` supplies `wasm-opt`. On non-macOS systems, install Binaryen with the
platform package manager before building.

Build the static web folder:

```bash
./scripts/build_web.sh
```

Create the itch.io upload archive:

```bash
./scripts/package_itch.sh
```

The package is written to
`target/itch/animal-fighter-club-web.zip`. The script verifies that
`index.html` is at the ZIP root, `pkg/` and `assets/` are sibling directories,
and the archive stays within itch.io's HTML5 file-count and size limits.

Serve locally:

```bash
python3 -m http.server 8000 --directory web_dist
```

Open `http://127.0.0.1:8000`, then click or press Enter to start user mode.

For itch.io, create an **HTML Game**, upload the ZIP, and select
**Click to launch in fullscreen**. The launch click supplies the browser
interaction needed before audio playback and controller joining. Test Xbox
controller detection on a draft upload in both Chrome and Safari before
publishing.

## Development documentation

- [Development workflow](docs/development.md)
- [Runtime architecture](docs/architecture.md)
- [Multiplayer architecture and delivery plan](docs/multiplayer-architecture.md)
- [Performance budgets and profiling](docs/performance.md)

## Controls

Setup:
- Enter: start the selected match
- 1: timed team score rules
- 2: free-for-all score rules
- 3: stock ring-out rules
- A: cycle arena selection forward
- Shift+A: cycle arena selection backward
- Demo battle is locked to one player fighter and one bot fighter.
- Z/X: cycle player/bot styles
- T/Y: cycle player/bot equipment
- R: reroll replay seed while staying in setup

Player:
- Arrow keys: move.
- Z: aim toward the bot; tap to grab.
- X: strong attack, throw held item, heavy throw during grab hold.
- C: light attack, pick up item when empty-handed, dash attack while dashing, jump attack while airborne, swing held item.
- V: jump, quick stand while knocked down.
- Double-tap a movement key: dash, recovery roll while knocked down.
- X + C: guard / block.
- While grabbed, X + C plus movement away: escape attempt; X + C: brace against throws.

User Mode Local Multiplayer:
- Choose single-player or two, three, or four local players from the mode screen.
- Join in P1-P4 order with Xbox A or any keyboard layout's Jump/Aim key; Xbox B leaves.
- Xbox: Left stick/D-pad move; A jump; X light; Y heavy; B aim/grab; RT dash;
  LB guard; LT ultimate; RB special.
- Open **Settings → Controls** to register or reorder local devices, remove or
  clear assignments, inspect live controller inputs, test vibration, and edit or
  restore all four keyboard layouts. Controller order is retained for the current
  session; keyboard bindings and vibration preference persist between launches.
- P1: Arrow keys move; Z/X/C/V actions.
- P2: A/D/W/S move; T/Y/U/I actions.
- P3: F/H/R/G move; B/N/M/Comma actions.
- P4: J/L/O/K move; 7/8/9/0 actions.
- Key Settings exposes all four layouts and swaps duplicate assignments safely.

Native Dev Hotkeys:
- Shift+U: enter user mode from the dev setup screen.
- In user mode, choose player count, one character per player, and an arena before the controls briefing.
- F2: toggle map editor while in setup.
- H: toggle hitbox, hurtbox, item, special, impact-source, reaction, technique-window, and feedback-cue debug overlays.
- Shift+Up/Down: pan the gameplay camera forward/back.
- Shift+Left/Right: rotate the gameplay camera.
- Shift+Cmd/Ctrl+Up/Down: raise/lower the gameplay camera.
- Shift+mouse wheel: zoom the gameplay camera.
- Shift+R: reset the gameplay camera.
- Shift+C: cycle gameplay camera filter/action-effect look.
- Shift+Cmd/Ctrl+F: toggle whether the saved single-player camera follows Player 1.
- Shift+Cmd/Ctrl+L: load the saved single-player camera preset into the live dev camera.
- Shift+Cmd/Ctrl+S: save the current gameplay camera angle as the single-player camera preset.
- Dev hotkeys are native-only and are blocked while user mode is active.

Debug:
- H: toggle hitbox, hurtbox, item, special, impact-source, reaction, technique-window, and feedback-cue debug overlays
- 1: timed team score rules
- 2: free-for-all score rules
- 3: stock ring-out rules
- R: reset/rematch with the current rules
- Enter on results: return to setup with the current selections

Pickups:
- Guard Battery: restores health and stamina.
- Foam Mallet: carried melee prop with four durability uses. Swing it or throw it.
- Pop Bomb: carried explosive. Throw it or drop it to arm a short fuse.
- Spark Lobber: ranged carried prop with a long light poke.
- Breeze Buoy: utility prop that consumes itself for stamina.
- Stone Crate: heavy prop with stronger close swings and throws.
- Guard Kite: shield-like prop with a short bash and high durability.

Specials:
- Pulse Dart: straight stamina projectile.
- Trip Plate: placed trap.
- Snap Wave: close expanding shockwave.
- Drift Field: short lingering hazard.

Styles:
- Anchor: slower, sturdier guard economy, stronger ring-control throws, a longer-invulnerable brace step, and stamina-paid heavy startup armor with longer whiff recovery.
- Vector: faster pressure movement and attacks, weaker guard economy, a shorter dash-attack flow, and a narrow dash-attack-to-light branch window.
- Catalyst: mid-range preference, stronger special usage bias, cheaper/faster special cycling, and stamina-disrupting special hits.
- Style identity is visible through compact HUD taglines and small in-match accent rings.

Equipment:
- Dash Coil: boosts dash attack knockback on cooldown.
- Aerial Spur: boosts jump attack damage on cooldown.
- Counter Cell: boosts perfect-guard counter damage on cooldown.
- Heavy Seal: boosts heavy attack knockback on cooldown.
- Equipment identity is shown through HUD effect text and a small fixed back-chip accent.

Guard Batteries are collected automatically by walking into them. Carried items
must be picked up with grab.

## Prototype Notes

- The demo starts as one controlled fighter versus one bot. Crown Ring keeps its original authored layout, while nine additional arenas use split terraces, layered stone steps, crossways, spirals, lanes, market wings, garden petals, snow steps, and cannon-court footprints.
- Arena definitions provide ground shapes, visual themes, spawn points, item anchors, ring-out bounds, camera hints, platform blocks, and phased hazard data. All ten arenas can be selected in user mode or cycled in native dev setup.
- The camera follows the center of living fighters from a high angled arcade view, while single-player user mode follows the controlled fighter with the saved single-player camera preset.
- Movement uses simple acceleration, friction, gravity, jump, dash stamina cost, radius-aware platform support, ledge jump grace, side pushout, limited wall bounce, and manual ground checks.
- Combat uses Rust-side technique definitions for startup, active, recovery, cancel/branch windows, stamina hooks, and impact payloads across the light chain, dash attack, jump attack, heavy attacks, guard counter, item actions, specials, and stateful grab/throw.
- Heavy hits, combo finishers, throws, thrown items, and Pop Bombs can launch fighters into reaction profiles, limited ground/wall bounce, knockdown, and get-up states.
- Knocked-down fighters wake automatically after a short pause and gain brief invulnerability while standing.
- Guard reduces front-facing damage and knockback, but stamina pressure can cause guard break. A precisely timed guard prevents chip damage and starts a quick counter.
- Guard plus dash performs a short defensive step, and knocked-down fighters can quick stand or recovery roll instead of waiting for automatic get-up.
- Primitive hit sparks, guard flashes, dash trails, dust puffs, respawn beams, and item effects make combat state easier to read without external assets.
- Simple arena items add arcade match chaos: stamina recovery, a carried melee prop, and a carried short-fuse bomb.
- Specials add a small projectile, trap, shockwave, and lingering hazard layer with stamina costs, cooldowns, owner grace, and reset cleanup.
- Arena hazards now include launch pulses, slowing snare fields, and bumper nodes with per-hazard phase offsets that apply neutral shared-impact pressure without awarding ring-out credit.
- HUD rows tint and show EDGE danger when fighters drift near arena ring-out bounds or fall toward the lower blast plane.
- Fighters use shared action rules with style tuning for movement, stamina economy, guard pressure, attack timing, throw pressure, and bot range preference. User mode supports one player against a bot or up to four local keyboard players.
- Equipped modifiers each affect one move, show effect text/cooldown status in the fighter HUD row, flash on trigger, route a feedback cue, and add a small visual accent.
- Move, item, special, style, and equipment tuning now live in internal Rust-side definition structs for faster iteration.
- Match flow now runs through countdown, fighting, time-up, results, reset/rematch, and return-to-setup phases.
- Rule presets cover timed team score, timed free-for-all score, and stock ring-out matches.
- Team-score rules now use centralized even-vs-odd team membership and explicit friendly-fire policy to block self/teammate strikes, grabs, item hits, bombs, and specials from awarding damage or ring-out credit.
- A lightweight local setup shell selects, previews, and applies the mode, arena, active fighter slots, fighter styles, equipment, and replay seed before countdown.
- Local match telemetry tracks ring-outs, uncredited falls, item hits, throw hits, guard breaks, and total damage for result/debug displays.
- Ring-outs happen when a fighter falls below the arena or gets pushed too far out. The last attacker gets a score point; stock rules also remove lives and can end the match.
- Bots pick the nearest opponent, move toward them, strafe, attack in range, and sometimes dash.
- Bots can chain light attacks, sometimes grab at close range, occasionally use dash or jump attacks, and make role-aware item choices: ranged pokes at distance, explosives near mid-range, utility when stamina is missing, heavy props for ring pressure, and defensive props for guarding.
- Bots now quick-stand or recovery-roll from knockdown, guard against readable incoming attacks, avoid active arena hazards/enemy specials/thrown items/armed bombs, and use style/equipment-aware personality weights with small deterministic mistakes.
- The HUD now shows phase/rule state, results, held item status, special/equipment cooldowns, debug overlays, timer, scores, health, and stamina.

## Next Steps

Use the [development workflow](docs/development.md) and
[performance protocol](docs/performance.md) when extending or optimizing the game.

- Continue platform collision tuning for walls, ramps, and ledges.
- Continue tuning balance, readability, and setup polish from live playtests.
- Continue tuning moving arena devices, map-authored overlay dressing, and footprint-aware bot navigation.
- Expand style and equipment identity so archetypes and modifiers change more than raw tuning.
