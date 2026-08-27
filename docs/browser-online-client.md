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
- Call `project_latest` from the same thread to apply the latest canonical
  snapshot and rollback-aware event sidecars to the rendered Bevy world.

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
