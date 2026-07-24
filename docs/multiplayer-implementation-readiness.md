# Multiplayer Implementation and Release Readiness

- Status: implementation evidence register; not yet a Steam release approval
- Audit date: 2026-07-24
- Target: first-release private/friends-only, unranked Steam listen play
- Architecture authority: [multiplayer-architecture.md](multiplayer-architecture.md)
- Product scope: [multiplayer-product-policy.md](multiplayer-product-policy.md)

This document overlays the architecture work packages with the implementation
that now exists in the repository. Checked tasks in
`multiplayer-architecture.md` mean a production boundary and local automated
evidence exist; they do not mean the corresponding measured or external
acceptance gate passed. Likewise, a source file or test being present here does
not mean the current commit passed a release run unless a dated release record
also says so.

## Readiness labels

| Label | Meaning |
| --- | --- |
| **Implemented + automated** | Production code exists and repository tests exercise the boundary without requiring Steam accounts. The final candidate must still pass the commands below. |
| **Implemented + external validation pending** | The production path exists, but acceptance requires real accounts, machines, networks, operating systems, or measured hardware. A fake backend is not release evidence. |
| **Product-deferred** | The general protocol may represent the capability, but the first-release policy intentionally hides and rejects it. This is not a defect in the private/friends listen milestone. |
| **Open implementation work** | A required production call site, input path, or composition boundary is still absent or under active implementation. |

## Architecture evidence matrix

| Architecture area | Readiness | Implementation evidence | Automated evidence and limitation |
| --- | --- | --- | --- |
| Fixed 60 Hz simulation, tick input and render separation | **Implemented + automated** | `simulation.rs`, `tick_input.rs`, `live_input.rs`, and the `FixedUpdate` set chain in `lib.rs` own canonical time and input. `components::SimPosition` owns rollback-relevant translation; `interpolation.rs` projects it one way into render `Transform`. `SimulationDriveMode::ExternalProjection` disables the render world's local canonical step for embedded or native online sessions. | Simulation, input accumulation, integer-tick hitstop, interpolation, render-transform perturbation, and embedded-client tests run under `cargo test`. The cross-platform workflow is configured to run the frozen stock tape, all 17 checked-in behavior tapes, and compact ten-arena content matrix on Linux, Windows, and macOS in debug and release profiles. A successful run for the frozen release candidate and physical Steam Deck execution still require attached external evidence. |
| Stable identity, deterministic ordering, bounded dynamic state and RNG | **Implemented + automated** | `ecs_identity.rs`, `determinism.rs`, `canonical_state.rs`, dynamic snapshots, and named gameplay seed derivation provide the canonical boundary. `contact_arbitration.rs` provides the bounded collect/sort/resolve contract. | Every gameplay contact-source family uses the central resolver. Identity allocation/reuse, canonical hash, overflow, contested-contact, and reversed ECS-allocation fixtures exist. Multi-target coverage spans generic Special, Bee, Chick, Penguin, item, ordnance, and arena-hazard sources; the generic Special/Chick/Penguin end-to-end fixtures additionally lock full event IDs, retained source lifecycle, and canonical target state. |
| Canonical snapshots, hashes and replay | **Implemented + automated** | `snapshot.rs`, the `live_*_snapshot.rs` modules, `state_delta.rs`, `replay.rs`, `replay_archive.rs`, and the listen-worker call site implement bounded canonical capture, restore, hashing, accepted-input tapes, result verification, retention, and atomic replay files. | Round-trip, corruption, restore, replay, archive idempotency/retention, and a production-listener save/load/headless-verify fixture exist. |
| Prediction, rollback, hard resync and smoothing | **Implemented + automated** | `predicted_client.rs`, `rollback.rs`, `client_protocol.rs`, `resync_transfer.rs`, `state_sync.rs`, `interpolation.rs`, and `presentation_projection.rs` own full-world prediction and correction. | Tests cover normal rollback, bounded hard resync, reordered transfer fragments, stale baselines, correction projection, reconnect snapshots, and final hash/result verification. |
| Rollback-safe events and presentation | **Implemented + automated** | `sim_event.rs`, the eight bounded presentation-intent journals, projection hooks, confirmed event routing, and Update-only consumers separate canonical outcomes from render/audio cues. The production headless composition does not install windows, cameras, render/audio assets, or presentation consumers. | Event-ID, deduplication, confirmation, projection, and per-journal rollback discard tests exist. `live_restore_discards_future_entries_from_every_predicted_presentation_journal` exercises the same production discard path used after prediction correction. Remaining hot-path allocation/performance work is tracked separately from presentation correctness. |
| Offline/local authority boundary | **Implemented + automated** | `local_loopback.rs`, `online_client.rs`, `authority_thread.rs`, and `UserModeState::network_match_requested` route player-facing local matches through a separate render-free authority/predicted world and bounded AFC protocol. The developer sandbox intentionally retains a direct local schedule. | Embedded loopback tests prove serialized startup, independent 60 Hz authority cadence during a render stall, projection, multi-seat ownership, and result confirmation. |
| Headless and listen authority | **Implemented + automated** | `headless.rs`, `live_authority.rs`, `authority_peer_hub.rs`, `remote_online_client.rs`, and `listen_authority.rs` compose the canonical authority. The listen host is a normal `RemoteOnlineClient` over a bounded in-process endpoint; remote peers use the same protocol over Steam endpoints. | Listen tests prove host/remote result parity, reconnect, render-stall independence, bounded lifecycle queues, and deterministic disconnected-bot input. The production-live acceptance test boots the real game composition without windows, cameras, render assets, or audio assets. |
| Protocol, UDP and deterministic network laboratory | **Implemented + automated** | `network_protocol.rs`, `network_codec.rs`, `network_runtime.rs`, `network_io.rs`, and `network_lab.rs` implement bounded messages, five channel semantics, in-process and ordinary UDP endpoints, reliability, and fault injection. | `network_lab_tests` covers ten simulated minutes over UDP plus typical/degraded/rollback-storm faults, ownership, malformed traffic, disconnect, bandwidth and queue bounds. `listen_authority::live_network_acceptance` runs the production headless/hub/four-client composition through Loopback, 600-tick Typical, Degraded repair, asymmetric RollbackStorm, and reconnect with exact parity, zero honest future-input rejection, and zero security violations. |
| Native Steam platform, lobby, authentication and listen P2P/SDR | **Implemented + external validation pending** | `steam_platform.rs`, `network_quality.rs`, `online_roster.rs`, `online_lobby.rs`, `native_online.rs`, and the custom auth-gated `steam_transport.rs` own callback pumping, metadata, invites/launch joins, rich presence, tickets, ownership, admission, P2P sockets, SDR initialization, quality, precommit RTT calibration and reconnect. The shipping runtime specializes one internal production core with the real Steam backend, auth signal channel and transport factory; its public API is unchanged. | In addition to bounded unit fixtures, two independent fake Steam backends drive the production core through create/join, exact-generation bidirectional auth, one physical P2P generation, identical committed manifest/config/roster, both rematch orderings, revision 2, a new `MatchId`/connection, completed retirement and stale-old-generation immunity. Each authenticated remote must contribute 20 valid readings to a connection-generation-specific 32-sample window; the authority commits the worst nearest-rank p95 as an immutable 2–6 tick delay and rejects incomplete, mismatched, or greater-than-12-tick rollback budgets. These tests cannot prove Valve backend behavior, actual relay selection, account licensing, overlay behavior, NAT traversal, or cross-machine teardown. |
| Player-facing native online application | **Implemented + external validation pending** | `native_online_app.rs`, `user_mode.rs`, `steam_platform.rs`, and `lib.rs` register the Online menu, private/friends creation, invite prompt, couch seat/loadout/team editing, readiness, countdown, calibration/quality/reconnect/results screens, listen/remote worker startup, local input submission, render projection, and Steam Input menu/gameplay action sets. Fatal terminal-worker and application-capacity transitions synchronously join active workers and clear session-local handoffs before exposing Error. Confirmed Results retain their worker/endpoint until the versioned owner-authored between-match reset. | Command-policy, couch-seat, invite, localization-key, keyboard/controller coexistence, stable controller ordinals, controller-only title-to-Online navigation, menu focus/bindings, gameplay mapping, exact calibrated-delay Start Match gating, and owner-link-only quality forwarding tests exist. `two_native_applications_reconnect_confirm_rematch_and_teardown_real_online_workers` drives two real online workers through reconnect, a normal result, more than the endpoint-drain quiet window on both sides, owner-first retirement with the remote still in confirmed Results, rematch, a second match, return and idempotent teardown. Coordinator fixtures independently cover client-first deferral and owner-first revision acknowledgement. This is automated fake-transport/application-boundary coverage, not cross-machine Steam composition evidence. Physical controller/Deck and real Steam evidence remain external release gates. |
| Reconnect, abuse isolation and confirmed results | **Implemented + automated** | `reconnect.rs`, `authority_input.rs`, `multiplayer_security.rs`, `network_runtime.rs`, `authority_peer_hub.rs`, `remote_online_client.rs`, `confirmed_progression.rs`, and the native application enforce identity/seat reclaim, neutral-to-bot substitution, bounded repair, ACK-tracked typed disconnects, bans, authority-confirmed idempotent results, and owner-authored between-match epochs. | Tests cover stale generations, revocation, platform bans, spoofing, malformed floods, peer isolation, exact terminal ACK/timeout and queue deferral, Steam endpoint-drain races, all typed client recovery dispositions, result retries, same-identity reconnect, atomic final-frame/Completed publication, Results keepalive without gameplay input, benign post-result close, and both rematch action orderings. The release-candidate workflow reruns the production live matrix plus explicit abuse/auth/reconnect cases in release profile before any platform package can build. Listen results remain explicitly untrusted. |
| Operations and performance | **Implemented local diagnostics boundary; candidate measurements and external ingestion pending** | `multiplayer_observability.rs`, `multiplayer_diagnostics.rs`, `replay_archive.rs`, and listen authority status provide bounded privacy-safe counters, audit records, server-tick distributions, async local export, restrictive atomic files, retention, complete replays, and fatal incident bundles. `performance.rs` provides repeatable profiling scenarios and allocation/RSS reporting. | Metric/archive/privacy/retention/authority-call-site fixtures exist. The same-hardware canonical-pose authority and rollback hot-path comparison is recorded in `performance.md`; it is development-reference evidence, not a shipping-hardware approval. Final schema-v4 graphical timing/allocation/soak matrices, minimum-supported-CPU and external GPU captures, and long Steam measurements remain release gates. A remote dashboard/upload service is an operational deployment and privacy-policy decision, not an in-process authority dependency. |
| Dedicated, ranked and trusted operation | **Product-deferred** | `afc-dedicated` is a render-free, untrusted all-bot smoke executable sharing the authority contract. First-release policy rejects hosted dedicated metadata, ranked play and trusted results. | The smoke command proves only local headless deployment. There is no Steam GameServer login, hosted-dedicated SDR listener, relay-ticket coordinator, player admission, ranked queue, trusted reward backend or shipping operator ban provider. These are deliberately outside the first private/friends listen milestone. |
| Browser distribution | **Implemented for local/offline; online is product-deferred** | `scripts/build_web.sh` creates the complete repository-root `web_dist/` artifact. | Browser-to-Steam networking is an architecture non-goal. A successful web build does not validate native online play. |

## Open implementation work

No required first-release private/friends listen Rust composition gap is currently
known. That statement is narrower than release readiness: the final candidate must
still pass the clean command matrix and all measured/external gates below.

## Implemented paths requiring external validation

These are not missing Rust implementations. They require a release candidate,
controlled accounts/hardware, captured logs and an explicit pass record.

- Two licensed Steam accounts on separate physical machines: private and
  friends-only create/join, invite overlay, invite launch and `+connect_lobby`.
- Bidirectional `ISteamNetworkingMessages` ticket/manifest bootstrap, AppID
  ownership callbacks, auth revocation, and sanitized failure display.
- Steam Networking Sockets P2P admission and confirmed SDR/relay routing through
  real NAT/firewall conditions; no direct-IP fallback is accepted as SDR evidence.
- One peer owning multiple couch seats, readiness, countdown, play, result,
  rematch, return-to-lobby and clean process shutdown.
- Disconnect, suspend/resume, cable/network loss, same-account reclaim, grace
  expiry, deterministic bot takeover and listen-host-loss no-contest.
- A successful Linux/Windows/macOS debug-and-release workflow run from the exact
  candidate, plus physical Steam Deck execution, with identical frozen-tape
  checkpoints and final result. The configured workflow is not itself a pass
  record.
- Cross-region and long-running Steam soaks, including quality warning/reject
  thresholds, queue stability, reconnect rate and bandwidth capture.
- Same-hardware final schema-v4 `FourBotStress`, `MapCycle100` and
  `Soak10Minutes` timing/allocation matrices, including authority tick
  distributions and live-allocation/RSS plateau review.
- Minimum-supported-CPU execution of the documented authority and rollback
  budgets.
- External GPU frame capture for the graphical stress and soak workloads; the
  in-process timing harness is not GPU-profiler evidence.
- Steam depot/package validation with the real AFC App ID. Spacewar 480 is useful
  only for development and is never shipping evidence.

## Product-deferred first-release capabilities

The following must remain unavailable and fail closed in the first-release build:

- public lobby discovery;
- mid-fight joining, spectators and listen-host migration;
- hosted Steam dedicated servers and GameServer SDR;
- ranked matchmaking, leaderboards, trusted results and valuable rewards;
- browser-to-Steam online networking.

Enabling any item above is a new product/security project. In particular, the
local `afc-dedicated` smoke executable must never be described as hosted Steam
dedicated acceptance.

## Build, run and verification commands

Run commands from the repository root. For every code change, the repository rule
still requires both `cargo run` and `cargo test`; focused commands do not replace
that final pair.

### Default native build

```sh
cargo build
cargo test
cargo run
```

The default feature set has no Steam backend. Its Online screen must report Steam
support unavailable while local/offline play remains functional. Exit the launched
game normally so `cargo run` completes successfully.

### Steam-enabled native build

Compilation and tests do not require a logged-in account:

```sh
cargo build --features steam-net
cargo test --features steam-net
```

Build a shipping-configured candidate by replacing `<IMMUTABLE_RELEASE_LABEL>`
and `<REAL_AFC_APP_ID>` with the release label and non-zero decimal App ID
assigned to this candidate. Launch the resulting binary from its Steam depot
without custom environment variables:

```sh
AFC_BUILD_ID=<IMMUTABLE_RELEASE_LABEL> AFC_STEAM_APP_ID=<REAL_AFC_APP_ID> \
  cargo build --locked --release --no-default-features --features shipping \
  --bin ffc-prototype
```

The explicit development-only Spacewar launch is:

```sh
AFC_STEAM_APP_ID=480 AFC_STEAM_DEV_SPACEWAR_480=1 cargo run --features steam-net
```

Do not set `AFC_STEAM_DEV_SPACEWAR_480` for a real App ID or release candidate.

### Dedicated deployment smoke

```sh
cargo run --bin afc-dedicated -- --smoke-ticks 120
```

Expected scope: a render-free, real-time, all-bot authority stops cleanly after at
least 120 observed ticks. This does not open a socket for players and does not
exercise Steam GameServer or hosted SDR.

### Deterministic network laboratory

```sh
cargo test --lib network_lab_tests -- --nocapture
cargo test --lib listen_authority::live_network_acceptance -- --nocapture
```

The first command runs the UDP/fault matrix. The second runs the actual headless
simulation, authority hub and remote client composition over in-process and
typical-fault transports.

### Web distribution

One-time toolchain prerequisites are documented in `README.md`. Build with:

```sh
./scripts/build_web.sh
```

The script must write the complete artifact only to repository-root `web_dist/`.
Serve exactly that output for local acceptance:

```sh
python3 -m http.server 8000 --directory web_dist
```

## Release-candidate decision rule

The first-release private/friends listen milestone is implementation-complete only
when the open implementation list is empty and the final clean command matrix
passes. It is Steam-release-ready only after that condition, a clean default and
`steam-net` build/test/run record, the external validation matrix, and the final
performance/security/operations review all pass. The product-deferred capabilities
stay disabled regardless of listen-path results. The casual listen grace and
bot-takeover policy are recorded; any future competitive mode must define its
stricter disconnect/forfeit policy before it can be enabled.
