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
4. Score legal targets with hysteresis and classify combat as neutral, advantage,
   disadvantage, hit confirm, guard pressure, wake-up, edge pressure, or resource
   recovery.
5. For `Standard`, forecast up to twelve legal actions against the three most likely
   delayed opponent responses and up to four authored follow-ups. The fixed arrays
   cap a replan at 144 allocation-free outcomes over a twenty-tick horizon.
6. Retain the selected target, tactic, expected response, and up to three plan steps.
   Resolve hit, guard, whiff, airborne, threat, unsafe, and timeout branches from
   authoritative contact state and authored technique windows.
7. Estimate bounded target velocity and steer into the selected move's predicted
   contact envelope before committing the input.
8. Retain each exact-action commitment until its authored fighter action is accepted
   and completes, unless rejection, invalidation, or immediate safety interrupts it.
9. Translate one semantic plan step at a time into the same press, hold, and release
   fields used by human-controlled fighters.

Planning code receives explicit snapshots, profile values, match seed, fighter ID,
and decision tick. It must not read Bevy ECS state, elapsed wall-clock time, or
presentation state directly.

## Fairness and Determinism

`Standard` delays recognition of opponent action and technique transitions by three
to five decision ticks. The delayed record retains its own elapsed time and authored
prediction facts, so a new raw transition cannot leak through recovery or cancel
timing before its reaction deadline. Response learning consumes only these perceived
transitions; it never reads `FighterInput`. Navigation may still reject unsafe ground
immediately so simulated reaction delay does not manufacture avoidable suicides.
Bots receive no damage, stamina, timing, or input privileges that players do not
receive.

Random variation uses counter-based samples derived from replay seed, fighter ID,
decision tick, a named stream, and a stable sample index. Adding a sample in one
behavior must not shift unrelated choices. Snapshot iteration and score ties use
stable semantic ordering rather than ECS query order.

The current game simulation still runs in variable-rate `Update`. Consequently,
the bot planner is deterministic for an identical snapshot/tick tape, but complete
cross-machine match replay requires the planned fixed-step gameplay migration.

## Tactical Forecasting and Memory

Hard gates handle participation, hitstop, forced developer controls, training
dummies, locked actions, knockdown recovery, and immediate hazards. Remaining
behavior uses utility scores for survival, stamina recovery, repositioning,
approach, pressure, punishment, disengagement, item collection/use, and objectives.

Target selection considers match legality, distance, recent threat, vulnerability,
and ring position. A target is retained until invalid or until a challenger exceeds
it by the configured switch margin. Free-for-all planning branches only over that
target; other fighters, specials, items, and hazards contribute a bounded external
threat cost. Items, objectives, navigation, developer overrides, and immediate
safety remain in their existing systems.

Direct attacks derive startup, active and recovery timing, contact shape, authored
motion, guardability, and stamina requirements from the technique catalog. Detached
Bee and Penguin skills expose prediction facts from the same module-owned constants
used by their runtime projectiles and placed attacks. The planner deliberately uses a
short reliable travel window rather than the maximum lock range so Standard remains
competent without becoming mechanically perfect.

Candidate utility includes health and stamina swing, initiative, arena position,
edge safety, expected contact time, recovery exposure, and whiff risk. The plan score
is expected utility minus the configured worst-case risk weight, plus a bounded
learned tactic bias and seeded jitter. Fast, low-risk attacks lead neutral play;
slower heavy attacks are reserved for advantage or punish windows. Grabs and delayed
strike-throw branches gain value only after perceived guarding, anti-air requires a
delayed airborne observation, and repeated dodge or retreat observations unlock an
approaching pursuit dash rather than a retreating bait dash.

Each opponent has a fixed 96-context response table keyed by move-relative range,
opponent state, edge pressure, and the bot's previous hit/guard/whiff result. Every
context begins with a count of two for attack, guard, grab, jump, dodge, retreat,
special, and wait. Counts decay by seven eighths every twenty decision ticks. Forced
states and automatic post-action returns to idle are excluded so hitstun is not
learned as a voluntary wait response.

Plans use `NeutralPoke`, `WhiffPunish`, `BaitAndPunish`, `StrikeThrow`, `AntiAir`,
`PressureString`, `EscapePressure`, `ProjectilePressure`, and `EdgeControl`. Legal
follow-ups come from the existing technique resolver, source-state predicates, cancel
windows, branch windows, and timelines rather than a second combo graph. Completed
tactics record hits, blocks, whiffs, damage and stamina swing, position change, and
initiative. The selected bias changes by `learning_rate * normalized_outcome`, all
biases decay toward zero, and every value is clamped to the profile cap. Response
counts, plan state, and tactic outcomes reset with the match seed.

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
target hysteresis, bounded variation, adaptation caps, utility weights, and the
strict tactical fields `tactical_planning_enabled`, `forecast_horizon_ticks`,
`forecast_risk_weight`, `tactic_learning_rate`, `tactic_bias_cap`, and
`near_optimal_margin`.

`Standard` defaults to `true`, `20`, `0.35`, `0.12`, `0.75`, and `0.25` for those
fields. `Tutorial` has tactical planning disabled and continues through the legacy
single-action planner. Both profiles have matching compiled fallbacks.

All fields are required, unknown fields fail parsing, numeric values are range
checked, and the complete catalog is replaced atomically only after validation.
Native builds poll for changes every 0.5 seconds. A missing or invalid initial file
uses compiled defaults; a failed reload retains the last valid catalog. Each bot
copies its normalized profile at match or behavior start, so hot reload never
changes an in-progress match.

## Diagnostics and Acceptance

Developer decision traces identify the selected target, goal, action, reaction gate,
commitment, reason score, tactical phase, tactic, top response probabilities,
forecast score, plan step, branch result, and learned bias without adding
player-facing UI. Click a bot in the native developer view, or set the 1-based
`AFC_BOT_TRACE_FIGHTER` environment variable for a harness run. Tests use fixed
snapshot tapes to cover named randomness, canonical ordering, target hysteresis,
delayed action/technique perception, conditional response updates, decay, stable
ties, scoring, invalidation, branch transitions, commitments, and input-edge
semantics.

The opt-in live quality probe runs deterministic combat fixtures and fails on button
spam or movement without authoritative action transitions, hits, and damage:

```bash
AFC_BOT_QUALITY_SCENARIO=Duel cargo run --features bot-quality
AFC_BOT_QUALITY_SCENARIO=FourBot cargo run --features bot-quality
AFC_BOT_QUALITY_SCENARIO=Tactics cargo run --features bot-quality
```

The live fixtures use Training Ground's continuous floor to isolate combat conversion.
The latest fixed-seed Duel passed with 35 accepted attacks, 23 damaging hit ticks,
and two action families. The four-character fixture passed with 167 accepted
attacks, 85 damaging hit ticks, damage from every fighter, three action families, and
no idle, stuck, or no-hit failure. Ring-outs and falls remain reported diagnostics
but are not required on the enclosed practice floor.

The allocation-free `Tactics` fixture runs every playable character across four
fixed seeds and scripted whiff, jump, retreat, guard, pressure, and passive contexts.
The latest run passed with 75.0% whiff punish, 68.8% anti-air, 100.0% safe escape,
75.0% legal hit-confirm follow-up availability, guard-counter selection increasing
from 0 to 32 after three guard observations, and failed bait selection falling 100%
between trial halves.

Every code-change batch must pass `cargo run` and `cargo test`. Changes affecting
the bot hot path also require same-hardware before/after `FourBotStress` captures
under the protocol in `docs/performance.md`. Web builds are produced only when
explicitly requested.
