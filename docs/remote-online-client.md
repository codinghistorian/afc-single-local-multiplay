# Remote Online Client Composition

`src/remote_online_client.rs` is the production client-side owner for one
admitted remote authority connection. It keeps the socket, protocol runtime,
predicted canonical world, rollback history, and clock estimator on a dedicated
60 Hz thread. The Bevy application remains a render/input consumer and never
steps that predicted world directly.

## Construction

Create `RemoteOnlineClient::spawn` only after platform authentication, lobby
admission, immutable roster agreement, and transport admission have completed.
Pass it:

- the admitted `NonBlockingDatagramEndpoint` (Steam, UDP, or in-process),
- the exact immutable `HeadlessMatchConfig` agreed in the lobby,
- the authenticated `PeerId`, and
- a validated `RemoteOnlineClientConfig`.

The admitted endpoint is installed on the worker, and the predicted Bevy world
is constructed and owned there. The
main-thread handle is transport-erased, so replacing Steam with the in-process
fault harness does not change gameplay/session behavior.

The worker deliberately remains in `Loading` until the application has loaded
every definition and presentation asset referenced by the accepted manifest and
calls `mark_content_loaded`. That release is an atomic, nonblocking signal; only
then may initial snapshot synchronization and readiness continue.

## Main-thread service contract

At the fixed input boundary, build a `RemoteLocalInputBatch` containing exactly
every local seat owned by the peer and call `submit_inputs`. The call is
nonblocking and returns `Queued`, `Full`, or `Disconnected`. The higher-level
`sample_local_inputs` helper drains `LocalTickInputState`; it retains and merges
button edges across a full command queue, so a render-thread stall cannot lose a
tap. Couch controllers are local ordinals within the peer and are explicitly
mapped to that peer's globally assigned protocol seats; global seat numbers are
never treated as local device indices.

The Steam transport owner may translate Steam connection telemetry into
`NetworkQualitySample` and pass it through `submit_quality_sample`. Clock-probe
RTT is sampled automatically; transport-provided loss completes the UI quality
view.

Call `project_latest` from the presentation owner. It applies the newest
canonical predicted snapshot to the rendered world, rewinds speculative event
sidecars when rollback occurs, and routes only the corrected event history.
Intermediate cosmetic ticks have a fixed history bound. The newest snapshot,
confirmed result, terminal status, failure, and quality/status record have
separate retained slots.

## Result and failure policy

`confirmed_result` remains empty until the protocol has both received the result
identifier and verified the exact final canonical tick/hash. Only that result is
passed to `ConfirmedProgressionLedger`. The first private/friends release has no
durable casual reward sink, so validated untrusted listen/offline records are
marked applied immediately and remain only in the bounded deduplication history.
They can drive the result presentation but cannot grant ranked, leaderboard, or
valuable progression. A future trusted-dedicated record remains pending until a
durable idempotent backend acknowledges its result key.

The exact final projection frame, retained result, `Results` status, and
`Completed` terminal are installed under one mailbox lock. Publishing Completed
does not end the worker: it keeps owning and pumping the admitted endpoint until
explicit match teardown, while refusing to submit any further gameplay inputs.
`stop`/join after Results preserves Completed instead of replacing it with a
synthetic Stopped terminal. If the authority retires the physical gameplay link
after the result is already verified, that transport-only close is benign and the
worker remains in Completed Results awaiting the between-match transition.
Protocol corruption or another non-transport fatal condition still fails closed.

Protocol, session, clock, transport, and simulation failures latch the worker
closed and publish an `OnlineFailure` containing only stable localizable codes.
An authenticated authority `Disconnect` for the exact active match is distinct:
the protocol lets `NetworkRuntime` queue its reliable ACK first, then atomically
publishes `RemoteAuthorityDisconnect` in status and terminal state with the worker
generation and local confirmed tick. A replacement generation starts with no such
context. The native application revalidates generation/match/role, retains the
first valid terminal over a later generic close, and maps the authority's retry
disposition to reconnect, lobby, no-contest, or menu recovery. No remote-authored
error text, detail code, or tick crosses into UI or becomes a reconnect seed. The
render thread can inspect `status`, `terminal`, and `confirmed_result` without
blocking.

## Reconnect

`reconnect` fully joins the old worker, increments the publication generation,
and starts a replacement worker with a newly admitted endpoint. It retains the
exact manifest and the authority-selected countdown start tick (which may be
later than the manifest proposal). The reconnect protocol accepts only the
authority's identity-bound reconnect snapshot/input-tail transfer, applies it
atomically, re-synchronizes the authority clock, and resumes input afterward.

The application/Steam coordinator remains responsible for:

1. detecting the transport close and showing reconnect status,
2. obtaining a newly authenticated/admitted Steam endpoint,
3. asking the authority to reserve the existing peer's seats with its last
   confirmed tick, and
4. passing the endpoint to `RemoteOnlineClient::reconnect` within the authority's
   reconnect grace window.

## Queue and thread invariants

- All cross-thread queues are bounded synchronous queues.
- Socket calls, command submission, status reads, and presentation notification
  are nonblocking.
- Local input is reticked/resequenced only on the worker using the synchronized
  authority clock.
- A failed input send does not consume a button edge or sequence number.
- One absolute-deadline scheduler drives both network pumping and prediction at
  60 Hz, independently of render FPS.
- Dropping or stopping the handle always signals and joins the worker; an already
  published Completed result remains the terminal after that join.
