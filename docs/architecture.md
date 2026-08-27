# Runtime Architecture

## Scheduling

Gameplay state advances through a deterministic fixed-step pipeline: input intent,
movement, attack generation, impact resolution, state transitions, and cleanup.
Frame-rate-dependent work samples input and presents the latest simulation state
through animation, effects, camera, rendering, and HUD systems. Dependencies are
declared between those stages; independent systems remain parallel.

Directional gameplay input modifiers run after all human and bot input producers
and before action interpretation. This keeps modifiers such as drunkenness
consistent across controller types without changing action-button semantics.

Local play uses the four stable fighter slots directly. Each human slot owns one
unique local keyboard or gamepad assignment; keyboard layouts retain independent
binding sets. User mode activates two to four human slots or one human plus a bot
without changing fighter entity identity, spawn order, camera tracking, combat
targeting, or replay ordering. Gamepad entities and device reconnect state remain
local presentation/input state and never enter authoritative match or replay state.

The Controls hub owns local device setup, family-aware menu conventions, live
input testing, and keyboard configuration. Device assignments are session state;
versioned keyboard and vibration preferences are stored in the platform
application-data directory on native builds and `localStorage` on web builds.
Unassigned setup cards use family-neutral Confirm/Back language. Once a controller
is identified, prompts use its physical labels, including DualSense Cross, Circle,
Square, Triangle, L1/L2, R1/R2, and Options.
Menu direction state is locked to one active controller until it returns to
neutral or disconnects, so another connected controller cannot reset its repeat
timer. Stick navigation emits once per neutral deflection with hysteresis; D-pad
navigation repeats only after the menu delay.
Single Player bypasses device setup, preserves an explicit P1 session assignment,
and otherwise starts on Keyboard 1. On eligible single-player screens, an
unassigned controller can request P1 through a controller-locked, two-press
confirmation. The reconnect state owns that modal, consumes its confirm/cancel
inputs, blocks underlying UI or gameplay, and synchronizes accepted assignments
to both session state and the live local setup. Disconnected eligible seats pause
combat or block menu input until the original or an unassigned replacement
controller reclaims them; both reconnect and takeover paths resume through a
one-frame input gate.

Controller identification runs only when a device connects or reconnects. Sony
vendor `0x054C`, standard DualSense product `0x0CE6`, DualSense Edge product
`0x0DF2`, and known native/browser names select the PlayStation family. The family
changes physical labels and menu conventions, while gameplay consumes one shared
normalized layout: LT/L2 aim, RT/R2 guard, RB/R1 dash, LB/L1 ultimate, and the
standard face buttons for jump, grab, light, and heavy attacks.

Windows and Linux use Bevy/Gilrs normalized gamepad mappings. Native macOS uses
Apple's extended GameController profile to expose controllers as Bevy `Gamepad`
components; this also avoids the raw HID profile used by some wired Xbox Series
controllers that macOS can enumerate without usable Gilrs elements. Web builds
consume standard Gamepad API mappings through Bevy. There is no custom HID or
Windows GameInput backend.

Controller haptics remain capability-based. Windows/Linux use Bevy rumble and web
uses a Gamepad haptic actuator only when the browser exposes one. Native macOS
uses Core Haptics: PlayStation prefers separate left/right handles, while Xbox,
Nintendo, and generic controllers use Apple's whole-controller default locality.
Missing or failed haptics update presentation metadata but never reject input,
joining, or reconnecting. Every assigned controller is mixed independently;
disconnect and replacement paths stop active requests and remove the old
controller before feedback can route to its replacement.

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
