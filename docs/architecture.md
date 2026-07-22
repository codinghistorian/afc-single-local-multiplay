# Runtime Architecture

## Scheduling

Gameplay state advances through a deterministic fixed-step pipeline: input intent,
movement, attack generation, impact resolution, state transitions, and cleanup.
Frame-rate-dependent work samples input and presents the latest simulation state
through animation, effects, camera, rendering, and HUD systems. Dependencies are
declared between those stages; independent systems remain parallel.

Inactive match states and map-specific behavior are gated with run conditions.
Systems should not retain unused query parameters or overlapping mutable queries,
because both reduce scheduler parallelism and can cause Bevy access conflicts.

## Ownership and caches

- The active arena resource is the sole authority for the selected map and is the
  invalidation signal for arena caches.
- The arena collision cache owns stable world-space barriers, support surfaces,
  rotations, bounds, and prop footprints. Movement, bots, and teleport mechanics
  query the same data that rendered props were generated from.
- A per-tick fighter snapshot supplies stable fighter ordering and shared position,
  velocity, team, and state data to combat, bots, cameras, and presentation.
- Render caches own reusable primitive meshes, materials, parsed authored map data,
  and asset handles. Arena setup references handles rather than recreating assets.
- High-frequency particles and simple skill visuals use bounded pools. Reset and
  despawn paths must release every active element and remove stale ownership data.

Cache invalidation must be explicit. Arena caches rebuild only on a map change;
fighter snapshots rebuild once per simulation tick; presentation caches rebuild
when their source hierarchy or loadout changes.

## Determinism

Fighters use stable slot order, impacts retain deterministic resolution order, and
seeded random streams are consumed only by their owning subsystem. Changes to query
iteration, event queues, pooling, or broadphase logic must be checked against replay
fixtures. Floating-point comparisons use the tolerance defined by the relevant
gameplay test, not a broader performance-only tolerance.

## Presentation boundary

Presentation observes gameplay and must not author authoritative combat state. HUD
and visual systems update only when source values change. Visual quality defaults to
`High`; lower quality tiers may reduce shadows or effect density only after an
explicit player choice and must not affect collision, timing, AI, or random state.

Static arena geometry should share materials and meshes where doing so preserves the
authored appearance. Before adding instancing, geometry merging, a spatial grid, or a
scene pool, first demonstrate that the relevant subsystem exceeds its threshold in
`performance.md`.
