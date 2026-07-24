# Multiplayer Operations and Diagnostics

- Status: bounded collection, local export, replay retention, and fatal-incident integration implemented
- Scope: listen authority, remote client, and headless dedicated authority
- Source contract: `src/multiplayer_observability.rs`, `src/multiplayer_diagnostics.rs`, and `src/replay_archive.rs`
- Authority integration: `src/authority_peer_hub.rs`

For the first release, “headless dedicated authority” in this runbook means the
untrusted all-bot deployment/test smoke harness only. Hosted Steam dedicated,
ranked play, player admission, and trusted result processing are disabled and
require separate shipping gates.

## Production invariants

Every online process keeps diagnostics outside canonical simulation state. Metrics
must never influence a tick, input substitution, rollback result, match result, or
state hash. Diagnostics may drive UI warnings and operator alerts only.

Audit records may contain the match, peer, seat, fighter, and simulation tick plus a
stable numeric reason code. They must not contain authentication ticket bytes,
Steam relay credentials, IP addresses, remote free-form text, platform persona
names, or packet payloads. The Rust audit types make those values unrepresentable.

The in-process audit ring retains 256 records and overwrites the oldest record on
capacity. Authority-step timing retains 1,024 samples and calculates p50, p95, p99,
maximum, and over-budget count without unbounded allocation.

The production authority worker calls
`pump_network_at(network_tick, monotonic_ms)` and records its complete service
duration with `observe_server_tick(duration_ns)`. Status/reporting code reads
`observability()` or `server_tick_distribution()`; those values are diagnostics
only. The older `pump_network(tick)` entry point remains deterministic for tests
and derives an audit timestamp from the tick. A regressing platform timestamp is
clamped for the audit ring and never blocks or rewinds canonical simulation.

The hub aggregates packet/byte deltas, accepted/rejected/substituted inputs,
queue/history high-water marks, hard resyncs, reconnects, reconnect-grace
expirations, and security actions. Its audit entries identify only
match/peer/seat/fighter/tick scopes plus stable numeric action and reason codes.
Disconnect and ban decisions are therefore actionable without storing raw
datagrams, addresses, tickets, credentials, or remote-controlled text.

Authority terminal delivery has separate bounded counters for queued, initially
deferred, acknowledged, timed-out, and transport-closed outcomes. Logical peer
isolation happens before this delivery state, so a slow ACK cannot submit gameplay
or delay another peer. The physical link remains only long enough to drain ordered
Control predecessors and the tracked terminal, with a 120-tick ceiling. Steam adds
transport-side endpoint-drain counters for started, 50 ms quiet-completed, and
250 ms hard-timeout cases.

Whole-match retirement is measured separately from endpoint drop. Each transport
counts `retirements_started`, `retirements_completed`, `retirement_timeouts`, and
`retirement_faults`; the whole-transport absolute cap is 300 ms. The coordinator
mirrors exact terminal outcome counts as `started`, `completed`, `timed_out`, and
`faulted`, and reports retirement-queue `high_water` against the fixed capacity of
four. A retirement pumps its bounded outbound budget before evaluating an exact
deadline, while receive/admission/event work remains disabled. These metrics are
lifecycle evidence, not permission to store the terminal payload, auth ticket, or
native diagnostic strings.

`AuthorityPeerHubMetrics::reconnect_grace_expirations` increments exactly once per
disconnected peer when its reclaim deadline is crossed. The matching
`ReconnectGraceExpired` audit record scopes the peer and exact effective simulation
tick; `value_a` is the retained seat bitmask and `value_b` is the configured grace
duration in ticks. From that boundary through match end, all of those seats retain
`DisconnectedBot` input origin and a later same-identity reclaim is rejected. The
signal records substitute-control policy only: it is not a forfeit, trusted result,
progression, or reward event.

## Shipping persistence boundary

Every production `ListenOnlineMatch::spawn` resolves one managed diagnostics root.
Set `AFC_DIAGNOSTICS_ROOT` to an absolute path for a managed installation. Relative
overrides are rejected, counted in `ListenAuthorityStatus::diagnostics`, and fall
back to the OS user-data location:

- macOS: `~/Library/Application Support/AFC/diagnostics`;
- Linux/Steam Deck: `$XDG_STATE_HOME/afc/diagnostics`, or
  `~/.local/state/afc/diagnostics` when XDG state is unset;
- Windows: `%LOCALAPPDATA%/AFC/diagnostics`.

If the platform user-data environment is unavailable, the final fallback is an
absolute AFC user-data directory below the OS temporary directory. No default
writes into the repository or process working directory. Managed/test deployments
may instead call `ListenOnlineMatch::spawn_with_diagnostics_root` with an explicit
absolute path.

The root contains `replays/`, `incidents/`, and `operational/`. On Unix, managed
directories are mode `0700` and files are mode `0600`. Publication writes a unique
temporary file, flushes it, hard-links it into its final protocol-ID filename
without replacement, removes the temporary file, and fsyncs the directory. An
identical pre-existing file is idempotent; the same identity with different bytes
fails closed. Persistence errors increment numeric status counters and never
change, clear, or replace a canonical result.

The authority loop performs no filesystem I/O. It sends immutable jobs to a
four-entry critical queue and a one-entry latest-periodic queue owned by
`afc-authority-diagnostics`. Periodic submission is nonblocking and drops with a
counter under backpressure. Canonical replay submission is nonblocking; a rare
full/disconnected submission is retained for terminal retry. After the 60 Hz loop
ends, terminal jobs may wait for the bounded writer, and shutdown joins it. If the
writer cannot be created, the same terminal jobs are attempted synchronously only
after canonical stepping has stopped.

Retention and size limits are schema constants, not operator-controlled unbounded
values:

| Artifact | Per-file maximum | Retained files | Retained bytes |
| --- | ---: | ---: | ---: |
| Replay (`.afcr`) | 64 MiB | 32 | 512 MiB |
| Fatal incident (`.afci`) | 2 MiB | 8 | 16 MiB |
| Operational snapshot (`.afco`) | 64 KiB | 16 | 1 MiB |

Managed directory scans stop after 128 matching entries and report a bounded
failure rather than scanning attacker-controlled storage without limit. The file
that was just published is never selected for retention pruning.

## Required external operations view

The repository implements bounded counters, privacy-safe local snapshots, replay
and incident export, and the read APIs needed to populate this view. It does not
deploy a dashboard, telemetry collector, alerting service, or upload path.
Production ingestion, retention outside the local bounds, operator access, and
player consent/privacy policy are external release decisions. Before rollout, the
approved operations system must present the following panels per process and build
compatibility ID:

- active lobbies, connecting peers, authenticated peers, active matches, and
  reconnect reservations;
- packets and bytes by channel/direction, reliable retries, queue depths, and queue
  high-water marks;
- endpoint-drain and whole-transport-retirement starts/outcomes, plus retiring
  transport queue high-water against its capacity of four;
- RTT, loss, relay route, quality class, late inputs, substitutions, and rejections;
- authority, predicted, and confirmed tick gap; rollbacks by depth and cause;
- full/delta snapshot bytes, hard resyncs, chunk retries, and confirmed hash
  mismatches;
- authority simulation p50/p95/p99/max and 1 ms budget violations;
- snapshot/history/dynamic-pool utilization and overflow counters;
- authentication rejection, malformed traffic, kick, temporary ban, disconnect,
  reconnect, reconnect-grace-expiry/permanent-bot, host-loss, and stable
  failure-code counts;
- confirmed result IDs and idempotent progression outcomes, never raw account data.

The external alerting system must implement:

- any confirmed hash mismatch after correction;
- any canonical pool/contact-buffer overflow in authored normal play;
- sustained authority p99 above 1 ms;
- monotonically growing history, queue, entity, or live-allocation count;
- repeated authentication/integrity failure for one peer or build;
- reconnect or hard-resync failure rate above the release baseline;
- any transport-retirement fault, repeated retirement timeout, or retirement queue
  high-water reaching its fixed capacity;
- public traffic reaching a build with public lobbies disabled.

## Incident capture

The listen authority creates its replay recorder from the exact immutable tick-zero
snapshot retained by `AuthorityMatch`. It records every returned canonical
`AuthorityTickReport` with the matching retained snapshot. Hash checkpoints are
written every 60 ticks and seek keyframes every 1,800 ticks. The recorder is taken
and finished exactly once when the report carries the canonical result ID; the
result replay is then published by the diagnostics writer.

On a fatal authority failure or abnormal command-channel loss, retain a bounded
incident containing only protocol IDs, compatibility/build/content hashes, manifest
hash, the latest canonical snapshot, the last 120 accepted-input ticks, the
256-entry privacy-safe audit tail, a sanitized numeric failure code, and bounded
worker/hub/security/observability/server-tick metrics. The incident schema has no
field for Steam IDs, authentication tickets, relay credentials, IP addresses,
backend error strings, persona names, free-form remote text, or packet payloads.

Operational snapshots use the same privacy-safe metric mirrors. A terminal snapshot
is always queued; a periodic snapshot is queued every 3,600 network ticks by
default. Set `operational_export_interval_ticks` to zero to disable only periodic
files, or to a value of at least 600 ticks. Replay and fatal/terminal persistence
remain active. These files are local evidence only. Upload or third-party ingestion
requires a separately reviewed product privacy/consent policy.

Correlate logs by match ID and stable peer ID. A reconnect keeps the same peer and
seat identity but receives a new connection generation. This lets operators
distinguish a late packet from the old transport from a protocol violation by the
new connection.

## Failure runbook

| Signal | Immediate action | Player-facing result |
| --- | --- | --- |
| Version/content mismatch | Reject before transport admission; compare published lobby metadata | Localized incompatible-build message |
| Steam auth/license rejection | End auth session and reject pending connection | Localized authentication/ownership message |
| Queue or callback pressure | Coalesce level-triggered Steam chatter; retire canceled operation slots and clean up delayed successful joins; reject outsider transport callbacks before capacity; fail only the attributable connection unless identityless bounded-state integrity is lost | Retry/reconnect where policy permits |
| Confirmed hash mismatch | Request one bounded full resync; disconnect after repeated repair failure | Reconnecting indicator, then stable desync error |
| Reconnect grace expired | Verify one expiry metric/audit record, retain deterministic authority-bot control for every owned seat, and reject later reclaim | Match continues with permanent bot replacement; no forfeit result or reward |
| Listen authority loss | Mark no-contest, stop accepting result/progression, retain the Steam lobby after owner transfer, and permit a fresh authority only after Return-to-Lobby | Host-left/no-contest message |
| Dedicated authority unhealthy | Stop admission, drain/terminate match according to deployment policy | Service-unavailable message |
| Malformed/rate-abusive traffic | Score the violation, kick/temporarily ban at policy thresholds | Stable kick reason; no remote text |

## Shipping evidence

Archive the exact release build IDs and outputs for the network fault matrix,
malformed-traffic suite, ten-minute and extended soaks, reconnect tests, Steam
invite/authentication manual matrix, cross-platform determinism tapes, bandwidth
capture, and before/after hot-path performance runs. Unsupported hosted-dedicated SDR
must remain reported as unavailable rather than silently falling back to exposed
public addressing. Local export proves bounded capture only; remote ingestion,
dashboard operation, alert delivery, access control, and the product
privacy/consent review require separate external evidence.
