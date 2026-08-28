# Browser online client

`BrowserOnlineClient<E>` is the browser counterpart to the native threaded
`RemoteOnlineClient`. It owns the same `RemotePredictedClientProtocol`, AFC
runtime, rollback history, and render-free predicted `LiveSimulationDriver`.
It does not create a thread, advance an authority, or let JavaScript callbacks
mutate simulation state.

## Execution contract

- Construct it only after the lobby service returns an authenticated peer and
  an exact `HeadlessMatchConfig`/manifest.
- Give it an endpoint implementing `NonBlockingDatagramEndpoint`. Browser
  transport callbacks may only place opaque `AfcDatagram` values in that
  adapter's bounded queues.
- Call `mark_content_loaded` after every manifest-referenced gameplay and
  presentation definition is resident.
- Merge local input with `sample_local_inputs` or `submit_inputs` before calling
  `service` for the current animation frame.
- Convert `performance.now()` to integer microseconds and pass it to `service`.
  The first call establishes a local epoch. The rational 60 Hz clock executes
  at most `max_fixed_steps_per_service` ticks in one call and retains every
  remaining due tick as backlog; it never drops or stretches simulation ticks.
  Catch-up ticks share the current real browser timestamp rather than inventing
  elapsed wall time. An authenticated clock reply whose measured RTT exceeds
  the estimator's one-second safety bound is discarded and immediately
  replaced; clock regression, probe mismatch, and identity mismatch still fail
  closed.
- Call `project_latest` from the same thread to apply the latest canonical
  snapshot and rollback-aware event sidecars to the rendered Bevy world.

The browser configuration drains at most seven inbound unreliable datagrams per
pump. That matches the committed-input relay's seven-tick redundancy window: a
backgrounded or briefly stalled tab cannot coalesce beyond the only window that
can close its next input gap. More queued traffic remains bounded and is handled
by later main-thread pumps; short catch-up bursts are backpressure, not an abuse
strike. The frozen stall fixture advances authority for 400 ms and proves the
client resumes without entering a repair loop.

The type carries an `Rc` marker and is deliberately neither `Send` nor `Sync`,
including in native tests. A regressed monotonic clock, incompatible manifest,
malformed authority traffic, exhausted retry budget, or invalid confirmed
result enters the same stable `OnlineFailure`/`RemoteOnlineTerminal` contract as
the native client. Authenticated authority disconnect payloads are retained
verbatim. Reconnect creates a new generation and predicted world, uses the
authority-selected countdown boundary, and requires a fresh endpoint and
authority snapshot.

## Frame-loop sketch

```rust,ignore
browser_client.sample_local_inputs(&mut local_inputs)?;
let report = browser_client.service(performance_now_micros());
let update = browser_client.project_latest(render_world)?;

if report.pending_fixed_ticks > 0 {
    // The configured bound protected this animation frame. A later service
    // call continues from the next exact 60 Hz tick.
}
if let Some(terminal) = update.terminal {
    route_online_terminal(terminal);
}
```

The client is transport-independent. WebTransport and WebSocket adapters must
preserve the 1,200-byte AFC datagram boundary and bounded queue semantics; they
must not decode or reinterpret canonical protocol messages.

## Browser application lifecycle

`browser_online_app.rs` owns the single-threaded product flow around this core:

1. Discover `/v2/config`, require the exact API/subprotocol/release identity,
   and restore a per-tab guest session when possible.
2. Keep lobby/chat/presence on the authenticated `afc.lobby.v2` control socket.
   REST mutations are revision-checked and never run from a simulation tick.
3. After the host freezes a ready roster, request a 30-second one-time ticket.
   Prefer WebTransport and fall back to WebSocket only when connection setup
   fails before admission.
4. Construct one `BrowserOnlineClient` on the browser main thread, sample only
   that guest's local seat, and project canonical snapshots into Bevy.
5. On an allowed disconnect, request a reconnect-scoped ticket carrying the
   latest confirmed tick and replace only the endpoint generation.
6. Present the authority-confirmed result, acknowledge it, release the projected
   match, and restore the same room. The room service clears readiness and may
   then freeze a new manifest for a rematch.

The HTML shell exposes no bearer or join ticket. Its frozen bridge accepts
typed bounded UI actions and returns a non-secret snapshot for accessibility and
automated QA. User strings are inserted with `textContent`, and a lobby resync
preserves the focused text draft rather than replacing partially typed chat.
