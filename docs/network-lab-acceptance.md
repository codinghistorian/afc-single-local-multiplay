# AFC Network Laboratory Acceptance Contract

This document is the executable acceptance contract for the pre-Steam transport
gate. Steam lobby, authentication, and SDR work must not replace or bypass these
tests. The same typed `NetworkRuntime` messages and the same canonical authority
and prediction boundaries are used for ordinary UDP, deterministic faults, and
future Steam adapters.

## Production boundary

`MultiPeerRuntimeCoordinator` owns at most four connection pairs. Each pair has
one client runtime and one authority runtime, while the match still has exactly
one `AuthorityMatch`. Inbound authority events are tagged with the authenticated
`PeerId` before the caller validates seat ownership and submits an `InputBatch`.
One peer may own multiple seats; the coordinator counts connections independently
from seats.

Each connection-local authority session gate sees only its remote peer. Global
readiness is therefore the conjunction of the local peer readiness records, not
`all_ready()` from any copied gate. After every peer is ready, the coordinator
selects one future boundary at or after both the manifest proposal and
`now + DEFAULT_COUNTDOWN_LEAD_TICKS`, broadcasts that identical countdown, and
rejects a second broadcast. A proposal that elapsed while a peer was loading is
not a startup failure.

`MeteredEndpoint` wraps any datagram endpoint and records successful sends and
delivered receives by AFC channel. Malformed envelopes are retained in a bounded
`unclassified` bucket. Metrics are observational and never feed simulation.

## Scenario matrix

| Test | Transport and conditions | Duration | Required evidence |
| --- | --- | ---: | --- |
| `net_loopback4` | Four ordinary `UdpEndpoint` loopback pairs | 36,000 simulation ticks (10 minutes) | Exact final hash/result parity, no hard resync, bounded queues |
| `net_typical4` | Deterministic 100 ms RTT, 20 ms jitter, 1% loss | 600 ticks | Final parity and every applied normal rollback at or below 12 ticks |
| `net_degraded4` | Deterministic 150 ms RTT, 30 ms jitter, 3% loss, plus low duplicate/reorder injection | 600 ticks | Final parity, bounded hard-resync recovery, useful fault counters |
| `rollback_storm` | One-tick upstream; nine-tick downstream plus jitter/reorder | 360 ticks | Repeated 9–12 tick corrections, no hard resync and no normal rollback beyond 12 ticks |
| `relay_gap_forces_bounded_hard_resync_and_recovers_final_parity` | Sixteen-tick delivery bursts | 180 ticks | Relay coverage gap is detected, reliable resync is applied, final parity returns |

The match sends an exact committed-input relay and one authoritative state update
per simulation tick. State updates are hashes on two ticks out of three and
snapshot deltas on the third tick, for an approximate 20 Hz delta cadence over a
60 Hz tick/input cadence. The committed relay uses the recommended low-bandwidth
starting point: a bounded five-frame newest-first tail (current plus four previous
frames) for every occupied seat. The protocol still accepts up to six previous
frames when field measurements justify the extra recovery coverage. RollbackStorm
uses that maximum seven-frame window while keeping the upstream leg inside the
negotiated input lead; this isolates downstream client rollback instead of mixing
it with authority missing-input substitution.

## Budgets

Steady-state byte rates are measured from the end of initial sync through the
authority's result tick. Reliable startup snapshot traffic is reported separately
and does not distort gameplay rate. Every peer must remain at or below:

- upstream: 16 KiB/s;
- downstream: 64 KiB/s;
- high-frequency datagram: 1,200 bytes (enforced by the runtime and codec);
- normal rollback: 12 ticks;
- prediction and input history: 64 entries in the lab, never above configured
  capacity;
- runtime inbound/outbound queue: 64 messages;
- reliable reorder window: 32 messages.

Per-channel application-wire bytes come from `MeteredEndpoint`. Fault-layer
metrics separately record injected duplicate copies, loss, reorder, delayed
delivery, token-bucket pressure, and disconnect attempts.

The forced sixteen-tick delivery-burst test is a recovery and queue-bound gate,
not a steady-state bandwidth profile: repeated full snapshots are its intended
load. The four named steady-state scenarios enforce the byte budgets above.

## Accepted deterministic baseline

The current four-peer baseline is:

| Scenario | Final hash | Drain | Hard requests | Upstream per peer | Downstream per peer | Maximum normal rollback |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| NetLoopback4 | `0x0f6f159e0116187b` | 0 ticks | 0 | 12,240 B/s | 45,603 B/s | 2 ticks |
| NetTypical4 | `0xaeae49b768c17032` | 4 ticks | 0 | 12,235 B/s | 45,526–45,530 B/s | 4–5 ticks |
| NetDegraded4 | `0x09c2def95d24faa9` | 8 ticks | 0 | 12,235 B/s | 45,527–45,530 B/s | 7–8 ticks |
| RollbackStorm | `0x7af96d09c24d89c3` | 11 ticks | 0 | 12,231 B/s | 59,182–59,186 B/s | 10–11 ticks |

These values are acceptance evidence, not protocol constants. Update the table
only after the matching deterministic test passes and an intentional cadence or
wire-format change explains the new baseline.

This table was revalidated on 2026-07-26 after the current five-frame committed
input relay contract was frozen. The complete release-profile network lab and
separate exact reruns reproduced every row. The earlier Typical and Degraded
literals were stale and did not reproduce under that final recovery contract;
the current Degraded run completes inside the normal rollback window and
therefore requires no hard resync.

## Reliable resync ordering

`ResyncBegin` is ordered reliable on Control while snapshot chunks and
`ResyncInputTail` are reliable on Resync. Cross-channel ordering is never assumed.
A client stages a chunk or input tail that legally arrives before Begin under these
strict limits:

- at most 128 chunks and 128 KiB globally;
- at most four unknown transfer identities;
- per-transfer chunk count cannot exceed the validated advertised count;
- duplicate indices must have byte-identical metadata and payload;
- duplicate input tails must be byte-identical and have one canonical window for
  every occupied seat;
- conflicting duplicates fail closed;
- the oldest transfer is evicted deterministically at capacity;
- pre-Begin data expires after at most 60 canonical ticks.

Staging allocates its fixed maximum at assembler construction, never from hostile
wire metadata. Tests cover complete chunk/tail-before-Begin assembly, conflicting
duplicates, unknown-transfer floods, deterministic eviction, and expiry.

The tail is mandatory before a transfer can complete. It is bound to the same match
ID, transfer ID, snapshot tick, and snapshot hash as Begin, and every occupied-seat
window ends exactly at the snapshot tick with no more than five contiguous records.
Seat/fighter/source ownership and canonical padding are validated before snapshot
application. Its encoded maximum remains below 1,200 bytes. The authority copies
the snapshot and committed histories as one preparation operation; tick zero uses
an explicit neutral substitute record for every occupied seat.

After application, only the newest record per seat seeds prediction's held-input
boundary. Older verified records extend committed coverage and never cause rollback
behind the snapshot. Reconnect follows the same rule and cannot resume input until
`ResyncApplied` plus fresh clock synchronization complete.

During Fighting, a client fences local input before requesting `HistoryExpired` or
hash-mismatch repair. The authority does not start an unsolicited transfer; an
unexpected Begin, chunk, or tail therefore fails closed. After a valid request,
bounded tail/chunks may still stage before Begin because reliable channels are
independent. The runtime retains all 128 possible chunk fingerprints for the active
and previous transfer, and the assembler retains fixed-capacity pre-Begin state, so
late byte-identical retries are ignored while conflicts fail closed.

For every locally owned seat, a requested repair preserves only the exact bounded
frames authored before the fence and replays them with identical sequence and edge
bits; it never re-authors an accepted tick. Authority-validated committed relays,
coverage, and pending state newer than the snapshot survive the rebase and advance
the client without regressing its confirmed frontier. Missing, inconsistent, or
over-capacity authored coverage fails closed. Reconnect is intentionally different:
its authenticated replacement connection begins a fresh authority-approved input
epoch, and replication received before atomic snapshot-plus-tail application is
validated then discarded.

When a verified reliable snapshot is overtaken by a newer confirmed state, the
client never regresses its world tick. It accepts the snapshot only as a newer
delta baseline (or idempotently ignores it when its existing baseline is newer),
then acknowledges receipt. Conversely, an in-flight delta referencing an evicted
baseline older than the client's explicit acknowledgement is obsolete and is
ignored; a genuinely missing current/new baseline requests a bounded full resync.
After `AuthorityResyncTransfer::validate_applied`, the authority immediately feeds
the applied `(tick, hash)` to `AuthorityStateSyncCoordinator`, avoiding a redundant
full snapshot while waiting for the next input batch to repeat that acknowledgement.

## Additional abuse and ownership gates

- `coordinator_preserves_couch_coop_ownership_on_one_connection` proves that two
  seats remain attached to one peer connection while other peers retain theirs.
- `malformed_datagram_flood_stays_within_runtime_bounds` sends arbitrary bounded
  datagrams and proves fixed queue high-water limits with no panic.
- `deterministic_disconnect_is_explicit_and_bounded` proves disconnect injection
  transitions both runtimes explicitly rather than silently simulating onward.
- `deterministic_bandwidth_bucket_delays_without_unbounded_queue_growth` proves
  token-bucket delivery respects its per-tick cap and queue bound.

## Production-live composition gate

`tests/support/live_network_acceptance.rs` supplements the transport-focused
laboratory with the actual render-free game composition. It boots a real
`HeadlessMatchConfig`, `LiveSimulationDriver`, `AuthorityPeerHub`, and
`RemoteOnlineClient`; crosses manifest agreement, chunked initial sync, clock
synchronization, countdown, movement and attack input, authoritative correction,
and reliable result confirmation; and projects the final verified snapshot.

Every committed network input is also stepped through an independently-built
live authority. The test compares its canonical hash at every reported tick and
compares the final authority, client, projection, and shadow hashes. Its five-case
matrix covers four-peer loopback for 36,000 simulation ticks, Typical for 600
active ticks, Degraded with loss/duplication/reordering and client-requested
repair, asymmetric RollbackStorm, and one-of-four reconnect. Queue/history and
bandwidth bounds, rollback depth, exact repair accounting, zero honest future-input
rejections, zero security violations, and the absence of windows, cameras, render
assets, and audio assets on the authority are assertions. Countdown lead exceeds
each profile's maximum one-way startup delay, and periodic clock refresh remains
valid after Fighting begins. The scenarios compare independent live runs rather
than freezing a literal checkpoint hash while behavior fixtures are being
versioned; the versioned fixture suite will own literal tapes and hashes.

The file is included as a test-only module so deterministic manual worker clocks
cannot leak into the shipping API. Run it while iterating with:

```sh
cargo test --lib listen_authority::live_network_acceptance -- --nocapture
```

Run an individual scenario while iterating with, for example:

```sh
cargo test --lib network_lab_tests::net_typical4 -- --nocapture
```

The repository-level `cargo test` gate remains authoritative before a handoff.

## Release-candidate automation

The protected `.github/workflows/release-candidate.yml` workflow has a
`release-acceptance` job between immutable-input preflight and every platform
build. Windows, SteamRT4 Linux, and macOS packaging cannot start unless that job
passes under Cargo's release profile. It runs:

- all five `production_live_*` composition scenarios serially;
- explicit malformed-datagram, peer-isolation, platform-ban, authentication
  revocation, stale-generation, transactional reconnect, and reconnect-result
  cases; and
- the 100,000-tick production-Bevy repeated-hash soak.

These automated gates are deterministic/fake-transport evidence. They do not
replace the separate real Steam account, SDR route, NAT, firewall, suspend, and
cross-machine acceptance record.
