# Multiplayer Product and Trust Policy

- Status: Initial online-release decision record
- Decision date: 2026-07-23
- Applies to: Steam native online play
- Architecture authority: [multiplayer-architecture.md](multiplayer-architecture.md)

This record closes the product decisions that gate WP7 and WP8. Network and
simulation code must fail closed if a session asks for a trust level or recovery
behavior that this policy does not permit.

## Launch scope

- The first public online milestone supports private and friends-only lobbies.
- Public lobby discovery remains disabled until fault-lab, Steam soak, abuse, and
  network-quality telemetry are stable in production-like testing.
- Online couch co-op is in launch scope. One authenticated Steam peer may own
  multiple local seats, subject to the four-fighter match cap.
- Mid-fight entry and listen-host migration remain out of scope. Steam selecting a
  new lobby owner never transfers simulation authority.

### Enforced first-release capability set

The player-facing native path has one fail-closed capability set:

| Capability | First release | Runtime enforcement |
| --- | --- | --- |
| Private lobby | Enabled | `NativeOnlineVisibility::Private` |
| Friends-only lobby | Enabled | `NativeOnlineVisibility::FriendsOnly` |
| Public discovery | Disabled | No native create action; metadata rejected by the coordinator |
| Listen authority | Enabled | Required at lobby entry, manifest commit/accept, and countdown |
| Hosted Steam dedicated | Disabled | Dedicated metadata rejected; hosted SDR reports unavailable |
| Ranked play | Disabled | No player-facing action or metadata claim |
| Trusted results/rewards | Disabled | `trusted_results == false` required at commit, accept, and countdown |

`FirstReleaseOnlinePolicy` is the code-level manifest gate. Lobby metadata has a
matching `validate_first_release_player_scope` gate. The coordinator checks both
before creating/entering a lobby, when freezing or accepting a manifest, and once
more before countdown. This redundancy is intentional: a future UI, test helper,
or callback-order change cannot turn an unsupported request into a different
session type.

## Authority and result trust

- Listen-authority matches are always marked unranked and untrusted.
- Ranked play, leaderboards, trusted rewards, and externally valuable progression
  are not in the first private/friends milestone.
- Any future trusted mode must use a dedicated authority, authenticate every Steam
  identity, and submit an idempotent authority-confirmed result identifier.
- Clients never submit scores, damage, inventory outcomes, achievements, or match
  results as authoritative facts.
- A duplicate result identifier with identical contents is accepted idempotently;
  conflicting contents are rejected and audited.

## Disconnect and reconnect

- A disconnected casual player has a 15-second (900 simulation-tick) reclaim
  window.
- During the first 2 seconds (120 ticks), the disconnected seats produce neutral
  input. From tick 120 until the reclaim deadline, the authority supplies
  deterministic bot input so the remaining players are not left with an inert
  fighter.
- A reconnect may reclaim only the same authenticated Steam identity, lobby
  membership, peer assignment, and seat set. Reauthentication, compatibility
  checks, and a fresh authoritative snapshot are mandatory.
- Successful reclaim disables bot takeover at a tick boundary. Inputs from before
  the accepted reclaim tick are rejected as late.
- At the exact 900-tick deadline, every seat owned by the disconnected peer remains
  permanently under deterministic authority-bot control for the remainder of that
  match. The immutable ownership manifest is not rewritten. This transition is
  applied once; an in-flight reclaim fails closed at the deadline, and the same
  identity cannot reclaim afterward.
- Permanent bot replacement is not a forfeit result and grants no result,
  progression, or reward. Listen matches remain untrusted. Competitive modes must
  define a separate, stricter disconnect/forfeit policy before they can be enabled.
- Loss of a listen authority ends the match as no-contest and returns surviving
  peers to the lobby. No result or progression reward is issued.

## Network-quality policy

- Matchmaking should prefer measured round-trip time below 100 ms.
- Sustained RTT from 100 through 150 ms displays a quality warning while retaining
  the normal prediction and rollback caps.
- Sustained RTT above 150 ms or loss above 3% enters degraded status and is exposed
  in the HUD and diagnostics.
- A peer is refused or disconnected when sustained RTT exceeds 250 ms, sustained
  packet loss exceeds 10% (1,000 basis points), traffic violates bounded rate
  policy, reliable retries are exhausted, or rollback cannot recover inside the
  configured hard-resync policy.
- One transient sample never changes policy. Production quality classification uses
  a bounded rolling window and hysteresis.
- Before a listen match is committed, each authenticated remote must provide at
  least 20 valid RTT readings in a connection-generation-specific 32-sample
  window. Unknown ping is not a zero-latency sample. The authority selects the
  worst peer's nearest-rank p95, converts it to one immutable 2–6 tick input
  delay, and refuses a start that would require more than 12 rollback ticks.
- Quality rejection is match-scoped. A listen owner isolates only the rejected
  peer and denies immediate same-match reclaim; Return-to-Lobby resets that
  decision. A client that rejects its owner link returns to the lobby instead of
  retrying the same route indefinitely.

## Admission and visibility

- Private lobbies require an explicit join token or invite.
- Friends-only lobbies require both valid lobby membership and the configured Steam
  relationship/visibility rule.
- Before countdown, every peer must match protocol, simulation, replay, gameplay
  content, build, authority kind, rules, arena, and seat metadata.
- Authentication or ownership failure, lobby removal, bans where enabled, version
  mismatch, full seat ownership, and expired join intent are explicit admission
  failures. They never fall back to anonymous play.
- Rich presence may advertise joinability only while the lobby is admitting peers.
  It is cleared during teardown; host-loss handling rebuilds it from the retained
  lobby's validated admission state (already closed during a committed match).

## Shipping gates

Public discovery, trusted results, and dedicated Steam deployment are separate
feature gates. Passing a listen-host Steam P2P smoke test does not enable any of
them. Each gate requires the matching acceptance scenarios, security checks,
cross-platform determinism evidence, performance measurements, and operational
telemetry defined by the architecture plan.

The `afc-dedicated` executable is currently an untrusted, all-bot,
render-free deployment/test smoke harness. It has no Steam GameServer login,
hosted SDR listener, player admission, ranked queue, or result backend. Its help
text, startup log, manifest, and capability constants identify that scope.
Successful construction or execution of this binary is not acceptance evidence
for hosted Steam dedicated, ranked, or trusted-result shipping gates.
