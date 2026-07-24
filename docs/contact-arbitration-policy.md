# Deterministic Contact Arbitration Policy

- Status: Accepted target behavior for the multiplayer simulation
- Decision date: 2026-07-23
- Implementation: Landed in simulation version 3 on 2026-07-23
- Applies to: combat hitboxes, specials, character skills, items, arena hazards,
  arena ordnance, grabs, and same-tick life loss
- Architecture authority: [multiplayer-architecture.md](multiplayer-architecture.md)

This policy replaces incidental ECS query and pool-allocation order with explicit
gameplay rules. It intentionally changes a small set of previously undefined
same-tick outcomes. Any later change is a simulation-version change and requires
updated permutation fixtures.

## Tick boundary

One gameplay tick uses the following contact boundary:

1. Read the complete accepted input frame for every active fighter.
2. Advance actions, fighter movement, stable dynamic objects, and arena devices.
3. Collect all geometrically valid potential contacts from the resulting pose.
4. Freeze the contact set. Do not re-run geometry after the first impact.
5. Deduplicate, sort, and arbitrate the bounded contact set.
6. Apply accepted impacts and source-specific hit bookkeeping.
7. Resolve all life losses as one batch, then determine the match result.

Eligibility such as active fighter, owner grace, invulnerability, and an existing
knockdown/grab state is sampled during collection. A reaction caused later in the
same contact batch does not erase an already-collected strike. This is what permits
real strike trades.

## Bounded contact identity

Every record contains only canonical values:

```text
ContactRecord
  phase
  source kind
  source ContactSourceId
  owner FighterId or neutral
  target FighterId
  payload / shape identity
  quantized impact priority
  contact ordinal
  quantized contact point and origin
  gameplay impact payload
```

`ContactSourceId` is a tagged canonical value. Dynamic sources use
`Entity(SimEntityId)` and retain generation/mapping validation. Persistent
authored hazards use `ArenaHazard { arena_index, hazard_index }`; both indices
must match the active immutable arena definition before resolution. Static
hazards never impersonate a recyclable entity-pool slot.

The buffer capacity is derived from fixed dynamic-pool and four-fighter limits. On
overflow, the simulation rejects the newest lowest-priority record by the same
canonical key and never allocates an unbounded fallback collection. The cumulative
overflow counter is runtime diagnostics and deliberately excluded from snapshots;
it cannot feed gameplay. The per-tick records and outcomes are transient, while
the cumulative counter may persist for the process lifetime. Retained contacts
still produce ordinary snapshot-hashed source/target mutations and lifecycle outcomes.
Normal authored content must have a fixture proving that it remains below the
limit.

The same `(ContactSourceId, target FighterId, contact ordinal)` may appear only
once. A duplicate with different payload is a deterministic simulation error rather
than a second hit.

## Geometry-only status rules

- A status contact carries `ContactPhase::Status` and no dummy `ImpactProfile`.
- Participation, owner exclusion, impact eligibility, and overlap are frozen by
  the collector exactly like damaging geometry.
- Central resolution validates the typed source and target, then records accepted
  status outcomes without damage, reaction, hitstop, combat event, or grab-role
  participation.
- The source consumer applies the status after the entire contact batch resolves.
  Barrel spray refreshes `DrunkStatus` for every accepted target and emits one
  source-level `AlcoholSprayed` lifecycle event with the canonical affected mask.

## Strike and damage rules

- All distinct, valid damaging contacts in the frozen set apply. A hit received by
  an attacker does not cancel that attacker's already-collected hit, so two fighters
  may trade.
- Contacts against one target resolve from lower to higher authored reaction
  priority. The strongest accepted reaction therefore owns the final motion/action
  state while every accepted contact still contributes its committed damage and
  telemetry.
- Equal reaction priority breaks by semantic source rank, owner `FighterId`, payload
  ID, shape ID, contact ordinal, and finally `SimEntityId`. Stable entity ID is only
  the final tie-break; records still tied before it must be gameplay-equivalent.
- The initial source rank is fighter strike, item melee/throw, character ability,
  generic special, arena ordnance, then persistent arena hazard. Rank controls only
  equal-priority final-state order; it never suppresses otherwise valid damage.
- Guard checks run in sorted resolution order. Multiple same-tick attacks may
  deplete guard stamina, and a later contact may become unguarded after an earlier
  guard break. This sequential rule is intentional and fixture-owned.
- Hitstop combines by maximum remaining duration and is therefore independent of
  contact order. Damage, score, and statistic additions use canonical quantization
  after the batch.

## Grab and cinematic-catch rules

- A grab or cinematic catch is a relationship claim, not an ordinary damaging
  strike.
- A claim fails when either its holder or victim has any accepted damaging contact
  in the same batch. Strikes therefore interrupt grabs without relying on which ECS
  object happened to run first.
- A fighter may be holder or victim in at most one accepted claim.
- Competing cinematic catches arbitrate before ordinary grabs. Otherwise the lowest
  holder `FighterId` wins, followed by payload ID, contact ordinal, and source ID.
- A failed claim still consumes any authored one-shot catch hitbox if its definition
  says the attempt is single-use; it does not mutate holder/victim relationships.
- Guarded cinematic catches follow their authored guarded-impact path and do not
  create a relationship.

## Dynamic-source bookkeeping

After arbitration, every source receives an outcome record keyed by its typed
`ContactSourceId`:
accepted, guarded, rejected-by-conflict, duplicate, or invalidated. Source systems
use that outcome to update `already_hit`, repeat windows, durability, pierce counts,
despawn-on-contact, child spawns, and semantic `SimEvent`s. Presentation never
participates in this feedback.

Destroying or recycling a dynamic source before its outcome is applied is a
deterministic error. A generation mismatch fails closed and cannot target the
object that reused the pool slot. Static hazard outcomes are validated against the
active arena definition and commit per-target cooldown only after acceptance.

## Contested pickup and arena rules

- Item pickup selects minimum quantized squared distance. An exact tie selects the
  lower `FighterId`; item iteration uses `SimEntityId`.
- Persistent hazards may hit every eligible fighter in a tick. Per-target cooldown
  is updated only for accepted contacts.
- A strike and neutral hazard can both land in one tick. Their final reaction order
  follows the same impact-priority and source-rank key.
- Bomb and projectile detonation is decided from the frozen pre-impact contact set;
  source despawn cannot erase another target already present in that set.

## Simultaneous life loss and result policy

Ring-outs and knockouts are collected and committed as one fighter-ID-sorted batch.
Stock decrement and valid attacker credit use the pre-batch participation snapshot.
The match result is calculated only after every loss in the batch is applied.

- If exactly one fighter/team remains eligible, it wins.
- If no fighter/team remains eligible after simultaneous final-stock loss, the
  result is a draw.
- Multiple credited losses in the same batch all award their valid attacker before
  result calculation.
- The result identity and canonical final state are emitted once by the authority
  after the batch reaches TickEnd. Progression remains a separate trust-gated,
  idempotent sink; untrusted listen results cannot grant trusted rewards.

This draw rule supersedes the legacy diagnostic in which ascending immediate
ring-out application could finish the match before the second final-stock loss was
credited.

## Required fixtures

Every fixture runs with forward/reversed fighter creation, permuted unrelated
presentation entities, and permuted dynamic-pool allocation where applicable:

- two-strike trade;
- two different-priority hits against one target;
- same-priority contacts from different source classes;
- guard depletion followed by a same-tick unguarded hit;
- grab versus strike, grab versus grab, and cinematic catch versus grab;
- two fighters claiming one item at equal quantized distance;
- hazard and strike against one target in the same batch;
- projectile/bomb contact with multiple targets;
- simultaneous ring-outs and final-stock draw;
- simultaneous respawn-space conflict;
- contact-buffer capacity and deterministic overflow.

Every permutation must produce identical canonical snapshots, event order, hashes,
and final result IDs.

The representative dynamic-source end-to-end fixtures include
`frozen_special_shockwave_is_independent_of_target_and_source_ecs_allocation_order`,
`frozen_chick_hazard_is_independent_of_target_and_source_ecs_allocation_order`,
and
`frozen_penguin_shockwave_is_independent_of_target_and_source_ecs_allocation_order`.
Each runs the production collect, central resolve, and source-outcome stages,
reverses fighter creation and source-before/after-target ECS allocation, and
compares the accepted target set, full ordered semantic events and IDs, retained
source hit memory/lifecycle, and resulting canonical target state. The existing
Bee fixture additionally exercises dynamic-pool permutation; the thrown-item
fixture locks single-consumption/durability behavior across reversed ECS
allocation.
