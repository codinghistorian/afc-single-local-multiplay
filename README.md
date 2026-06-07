# FFC Prototype

Runnable Rust/Bevy prototype for an original low-poly 3D arcade arena brawler. It uses Bevy primitives only: no external art, no copied layouts, and no third-party physics crate.

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
```

Build the static web folder:

```bash
./scripts/build_web.sh
```

Serve locally:

```bash
python3 -m http.server 8000 --directory web_dist
```

Open `http://127.0.0.1:8000`, then click or press Enter to start user mode.

## Controls

Setup:
- Enter: start the selected match
- 1: timed team score rules
- 2: free-for-all score rules
- 3: stock ring-out rules
- A: cycle arena selection
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

Native Dev Hotkeys:
- Shift+U: enter user mode from the dev setup screen.
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

- The demo starts as one controlled fighter versus one bot in a circular stone arena with red target markings, side blocks, a rear primitive billboard, and a dark void.
- Arena definitions now provide spawn points, item anchors, ring-out bounds, camera hints, platform blocks, and hazard marker data. Crown Ring, Split Causeway, Low Tide Steps, and Crank Yard can be selected from setup.
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
- Arena hazards now include launch pulses, slowing snare fields, and bumper nodes that apply neutral shared-impact pressure without awarding ring-out credit.
- HUD rows tint and show EDGE danger when fighters drift near arena ring-out bounds or fall toward the lower blast plane.
- Fighters use shared action rules with style tuning for movement, stamina economy, guard pressure, attack timing, throw pressure, and bot range preference. The current demo exposes one GetAmped-style player control layout against one bot.
- Equipped modifiers each affect one move, show effect text/cooldown status in the fighter HUD row, flash on trigger, route a feedback cue, and add a small visual accent.
- Move, item, special, style, and equipment tuning now live in internal Rust-side definition structs for faster iteration.
- Match flow now runs through countdown, fighting, time-up, results, reset/rematch, and return-to-setup phases.
- Rule presets cover timed team score, timed free-for-all score, and stock ring-out matches.
- Team-score rules now use centralized even-vs-odd team membership and explicit friendly-fire policy to block self/teammate strikes, grabs, item hits, bombs, and specials from awarding damage or ring-out credit.
- A lightweight local setup shell selects, previews, and applies the mode, arena, one bot opponent, fighter styles, equipment, and replay seed before countdown.
- Local match telemetry tracks ring-outs, uncredited falls, item hits, throw hits, guard breaks, and total damage for result/debug displays.
- Ring-outs happen when a fighter falls below the arena or gets pushed too far out. The last attacker gets a score point; stock rules also remove lives and can end the match.
- Bots pick the nearest opponent, move toward them, strafe, attack in range, and sometimes dash.
- Bots can chain light attacks, sometimes grab at close range, occasionally use dash or jump attacks, and make role-aware item choices: ranged pokes at distance, explosives near mid-range, utility when stamina is missing, heavy props for ring pressure, and defensive props for guarding.
- Bots now quick-stand or recovery-roll from knockdown, guard against readable incoming attacks, avoid active arena hazards/enemy specials/thrown items/armed bombs, and use style/equipment-aware personality weights with small deterministic mistakes.
- The HUD now shows phase/rule state, results, held item status, special/equipment cooldowns, debug overlays, timer, scores, health, and stamina.

## Next Steps

See [docs/prototype_todos.md](docs/prototype_todos.md) for the reference-informed
combat roadmap and planning backlog.

- Continue platform collision tuning for walls, ramps, and ledges.
- Continue tuning balance, readability, and setup polish from live playtests.
- Add richer arena hazard patterns and bot avoidance.
- Expand style and equipment identity so archetypes and modifiers change more than raw tuning.
