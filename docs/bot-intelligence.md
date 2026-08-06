# Bot Intelligence Architecture

## Goals

The bot system provides a fair, readable opponent for a broad casual audience
while keeping decisions reproducible and reusable by future match formats. Bots
control fighters only through `FighterInput`; combat, movement, damage, scoring,
items, and ring-out rules remain authoritative in their existing systems.

The first supported roster remains the fixed four fighter slots. Shared sensing,
navigation, and stateless decision inputs are designed so a later authority-owned
simulation can run several bots without duplicating world scans. Larger rosters,
team coordination, persistent learning, and full fixed-step gameplay are separate
projects.

## Runtime Pipeline

Bot input runs at the existing input-system boundary before tutorial and gameplay
input modifiers:

1. Build one canonical snapshot of participating fighters, items, specials, active
   hazards, and arena state.
2. Advance a 20 Hz integer decision clock. Missed epochs advance the tick but are
   collapsed into one fresh decision rather than replaying input bursts.
3. Refresh bounded perception and per-opponent tendency memory.
4. Score legal targets with hysteresis, then score tactical goals and legal actions
   using authored technique timing, range, stamina, facing, and recovery facts.
5. Estimate bounded target velocity and steer into the selected move's predicted
   contact envelope before committing the input.
6. Retain an offensive commitment until its exact authored fighter action is accepted
   and completes, unless rejection, invalidation, or immediate safety interrupts it.
7. Translate semantic intent into the same press, hold, and release fields used by
   human-controlled fighters.

Planning code receives explicit snapshots, profile values, match seed, fighter ID,
and decision tick. It must not read Bevy ECS state, elapsed wall-clock time, or
presentation state directly.

## Fairness and Determinism

`Standard` delays recognition of opponent actions by three to five decision ticks.
Navigation may still reject unsafe ground immediately so simulated reaction delay
does not manufacture avoidable suicides. Bots receive no damage, stamina, timing,
or input privileges that players do not receive.

Random variation uses counter-based samples derived from replay seed, fighter ID,
decision tick, a named stream, and a stable sample index. Adding a sample in one
behavior must not shift unrelated choices. Snapshot iteration and score ties use
stable semantic ordering rather than ECS query order.

The current game simulation still runs in variable-rate `Update`. Consequently,
the bot planner is deterministic for an identical snapshot/tick tape, but complete
cross-machine match replay requires the planned fixed-step gameplay migration.

## Utility Decisions and Memory

Hard gates handle participation, hitstop, forced developer controls, training
dummies, locked actions, knockdown recovery, and immediate hazards. Remaining
behavior uses utility scores for survival, stamina recovery, repositioning,
approach, pressure, punishment, disengagement, item collection/use, and objectives.

Target selection considers match legality, distance, recent threat, vulnerability,
and ring position. A target is retained until invalid or until a challenger exceeds
it by the configured switch margin. Action candidates use the existing combat and
item facts rather than parallel damage or timing constants.

Direct attacks derive startup, active and recovery timing, contact shape, authored
motion, guardability, and stamina requirements from the technique catalog. Detached
Bee and Penguin skills expose prediction facts from the same module-owned constants
used by their runtime projectiles and placed attacks. The planner deliberately uses a
short reliable travel window rather than the maximum lock range so Standard remains
competent without becoming mechanically perfect.

Candidate utility includes predicted contact time and recovery confidence. Fast,
reliable attacks lead neutral play; slower heavy attacks gain value against vulnerable
targets, and grabs gain value against observed guarding. Seeded candidate jitter is
applied before selection, so variation can change a close decision without overriding
legality or safety.

Each bot retains a bounded eight-second tendency history for opponent aggression,
guarding, grabs, jumping/dodging, repeated openers, and spacing. Adaptation changes
strategy weights only, is capped by the profile, and resets each match. It never
changes reaction time, accuracy, damage, stamina, or game rules.

## Navigation

Arena navigation is shared, footprint-aware, and built from the existing arena
support and collision sources. Static topology is cached once in stable node order;
runtime path searches reuse bounded scratch storage. Active hazards and moving
devices are applied as dynamic blockers or costs, and the next movement segment is
revalidated against current geometry before use.

Paths are recomputed when a goal or waypoint becomes invalid or when progress
stalls. Flow fields, formations, crowd avoidance, and moving-platform prediction
are intentionally deferred.

## Profiles and Tuning

`assets/bots/bot_profiles.ron` contains complete `Standard` and `Tutorial` profiles.
The schema covers reaction and commitment limits, perception error, safety margins,
target hysteresis, bounded variation, adaptation caps, and utility weights.

All fields are required, unknown fields fail parsing, numeric values are range
checked, and the complete catalog is replaced atomically only after validation.
Native builds poll for changes every 0.5 seconds. A missing or invalid initial file
uses compiled defaults; a failed reload retains the last valid catalog. Each bot
copies its normalized profile at match or behavior start, so hot reload never
changes an in-progress match.

## Diagnostics and Acceptance

Developer decision traces identify the selected target, goal, action, reaction
gate, commitment, and reason score without adding player-facing setup UI. Tests use
fixed snapshot tapes to cover named randomness, canonical ordering, target
hysteresis, reaction delay, adaptation bounds, commitments, and input-edge
semantics.

The opt-in live quality probe runs deterministic combat fixtures and fails on button
spam or movement without authoritative action transitions, hits, and damage:

```bash
AFC_BOT_QUALITY_SCENARIO=Duel cargo run --features bot-quality
AFC_BOT_QUALITY_SCENARIO=FourBot cargo run --features bot-quality
```

Both fixtures use Training Ground's continuous floor to isolate combat conversion.
The latest fixed-seed Duel passed with 43 accepted attacks and 22 damaging hit ticks.
The four-character fixture passed with 186 accepted attacks, 141 damaging hit ticks,
damage from every fighter, four action families, and no idle, stuck, or no-hit failure.
Ring-outs and falls remain reported diagnostics but are not required on the enclosed
practice floor.

Every code-change batch must pass `cargo run` and `cargo test`. Changes affecting
the bot hot path also require same-hardware before/after `FourBotStress` captures
under the protocol in `docs/performance.md`. Web builds are produced only when
explicitly requested.
