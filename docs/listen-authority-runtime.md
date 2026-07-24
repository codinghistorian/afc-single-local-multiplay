# Listen-authority runtime

- Status: implemented production listen composition; live Steam validation pending
- Source: `src/listen_authority.rs`, `src/multiplayer_diagnostics.rs`, `src/replay_archive.rs`, `src/native_online_app.rs`

`src/listen_authority.rs` is the production composition boundary for a casual
Steam listen match. It is created only after the lobby has committed an exact
`HeadlessMatchConfig` and an authenticated, fixed peer roster.

## Ownership

- `afc-listen-authority-60hz` is the only thread that owns the render-free
  `LiveSimulationDriver` and `AuthorityPeerHub`.
- The application thread owns `ListenAuthorityWorker` and the host's
  `RemoteOnlineClient`. It never reads or mutates the authority Bevy world.
- The host uses a bounded `InProcessEndpoint` pair. Its client performs the same
  handshake, manifest acceptance, initial resync, clock sync, countdown, input,
  correction, reconnect, and result verification as a remote client.
- Remote peers use `SteamDatagramEndpoint`. `ListenDatagramEndpoint` erases the
  transport without heap-allocated trait objects, so one hub can own both kinds.

The authority loop uses absolute 60 Hz deadlines independent of rendering. Each
iteration services a bounded number of lifecycle commands, pumps every peer,
advances at most one canonical tick, pumps queued state/result traffic again,
and publishes one latest-wins status.

## Startup and endpoint handoff

1. Freeze the manifest and authenticated roster in the lobby coordinator.
2. Construct `ListenAuthenticatedRoster::new(&match_config, host, peers)`.
   Construction rejects missing/extra manifest peers, duplicate platform users,
   duplicate peer IDs, invalid identities, and a missing/mismatched host.
3. Call `ListenOnlineMatch::spawn(match_config, roster, authority_config,
   host_client_config)`. The call returns only after the authority world exists
   on its worker and the host authority endpoint is attached.
4. Mark the returned `host_client` content-ready after local assets/definitions
   have loaded.
5. For each admitted Steam endpoint, call
   `authority.try_attach_initial(peer_id, user_id, endpoint)`. Never pass an
   endpoint before Steam authentication and lobby membership revalidation.

All submissions are nonblocking and return `Queued`, `Full(command)`, or
`Disconnected(command)`. A full result returns ownership of the endpoint so the
coordinator can retry or close it intentionally. Attach outcomes are reported as
bounded `ListenAuthorityEvent` values; malformed lifecycle requests do not end
the match.

### Lobby identity epochs and churn

`OnlineLobbyEvent::RosterChanged` carries one bounded snapshot of every
coordinator-reserved `(SteamUserId, PeerId)` tuple. This event is the
cleanup-before-reallocation barrier: the native runtime reconciles authenticated
mappings, tickets, reconnect markers, endpoints, and pending manifest handoffs
before forwarding it, and the application then reconciles its bindings and
staged authority work before it can consume a later authentication event.

Before manifest commit, a Steam member that is absent from the refreshed lobby
roster is synchronously removed from these layers. On return to lobby, gameplay
endpoints are dropped first and every remote tuple from the old match is removed
before `ReturnedToLobby`, even if that Steam member is still present and will
authenticate again for a rematch. After manifest commit, the fixed tuple remains
reserved across temporary membership loss so same-identity reconnect cannot be
replaced by a new participant.

Quality and malformed-signal quarantine remains fail-closed while the rejected
user is an active lobby member and throughout a committed fixed match. Before
commit, once the authoritative Steam roster proves that user departed, roster
admission already rejects further traffic, so its bounded quarantine entry is
reclaimed. A committed lease keeps its quarantine entry and is never reopened by
roster churn. This prevents sequential pre-match departed identities from
exhausting a long-lived lobby's fixed-capacity security bookkeeping without
weakening fixed-match isolation.

## Disconnect and reconnect

Call `authority.try_detach(authority_connection)` when the platform closes a
connection. The application retains a fixed mapping from each admitted
`SteamConnectionId` to the exact `AuthorityConnectionId` returned by the attach
event. Detach and authentication-revocation commands carry that generation
token, not only a peer or user identity. A delayed, retried command for a retired
generation is therefore an idempotent stale no-op and can never detach or revoke
a reconnect replacement. Platform bans remain intentionally user-wide.

An endpoint transport failure is also detected by the peer hub and detached
there. The hub applies its exact reconnect policy: fully neutral committed
frames first, then `deterministic_disconnected_bot_frame`, followed at grace
expiry by permanent deterministic bot control for the rest of the match. The
bot tape is a pure function of match seed, authenticated peer, seat, and tick
and is therefore canonical and replayable. The immutable ownership manifest is
retained, but later reclaim is rejected.

After Steam authenticates the same identity on a replacement connection, call:

```text
authority.try_attach_reconnect(user_id, reconnect_claim, authority_endpoint)
remote_client.reconnect(client_endpoint)
```

The claim uses the client's last confirmed tick. The authority reserves the
same seats, transfers an atomic snapshot plus committed input tail, performs a
fresh clock synchronization, and only then admits new inputs. The retained
actual countdown boundary—not the lobby's minimum proposal—is reused.

All resync request ticks are peer-authored and are checked against the authority's
current retained snapshot before transfer construction. A request claiming a
future confirmed tick is recorded as an attributed protocol violation and detaches
only that connection; it cannot surface `RequestAheadOfSnapshot` as an
authority-global worker failure or interrupt healthy peers.

State-baseline acknowledgements are also verified against authority history before
they can change a peer's delta base. Future ticks and retained-tick hash mismatches
are attributed protocol violations. An acknowledgement for an expired tick is
unverifiable and is ignored without replacing the last verified baseline. If that
retained baseline has itself expired, hash replication makes the client fence input
and request one bounded repair; the authority never pushes an unsolicited in-fight
snapshot across an unknown input generation. A validated `ResyncApplied`
acknowledgement authoritatively replaces provisional peer acknowledgement state for
that completed transfer.

Peer-requested fighting repairs are limited per immutable roster peer: by default
one request may start after a two-second cooldown and no more than three may start
in a rolling minute. The budget survives reconnect links. Initial/reconnect sync
and duplicate requests coalesced into an existing transfer bypass it. A first
cooldown denial is consumed as an explicit peer-local
rate violation rather than a forced session-transition disconnect. Continued
cooldown/window exhaustion accrues normal policy score and eventually isolates
only that peer without stopping authority ticks or healthy peer delivery.

## Application observations

`authority.status()` is lock-bounded and latest-wins. It includes:

- network and canonical authority ticks;
- the authority-selected countdown boundary;
- per-peer connection generation and protocol phase;
- the confirmed authority result identifier/tick/hash;
- hub and worker queue/timing metrics;
- numeric replay/incident/export queue and persistence counters;
- a retained terminal failure, if any.

`authority.drain_update()` additionally drains the bounded lifecycle-event
mailbox. Identity, exact-generation terminal, and shutdown facts use a dedicated
non-evicting lane. Telemetry/rejection notifications use a separate lossy lane.
If the lifecycle lane saturates, the worker fails closed with a retained
terminal instead of evicting a fact and allowing application/transport ownership
to diverge. Results and terminal state are retained independently of notification
coalescing. Rendering may stall or skip every intermediate status without
changing authority cadence.

Normal listen-owner teardown is a monotonic drain:

1. Submit `NativeOnlineCommand::QuiesceAdmission`; verify the monotonic admission
   fence while retaining established outbound endpoint drain.
2. Submit `authority.try_begin_graceful_shutdown()`.
3. Keep the authority, host client, remote endpoints, and runtime alive while
   pumping terminal messages and acknowledgements.
4. The authority freezes canonical advancement, publishes exact
   `TerminalDrained` facts for retired generations, and reaches `Drained` after
   all acknowledgements or its bounded timeout.
5. Join the worker, complete the requested rematch/lobby/menu transition, then
   retire the transport owner.

The application uses this path for rematch, return-to-lobby, leave, menu/retry,
coordinator endpoint drop, and process exit. The immediate stop/join operation
and `Drop` remain bounded emergency fallbacks. A result does not automatically
destroy the authority endpoint; the application/lobby coordinator chooses the
transition and begins the same drain.

## Replay and diagnostic shutdown

Production `spawn` enables the managed diagnostics boundary described in
`multiplayer-operations.md`. The authority records only its actually committed
input reports and their matching canonical snapshots. The completed replay is
queued once when the canonical result first appears; result delivery and the
authority lifecycle do not depend on persistence success.

Fatal worker failures and abnormal command-channel loss create a bounded local
incident. Normal requested stop does not create an incident, but it does create a
terminal operational snapshot. Periodic operational export uses a one-entry
nonblocking queue and cannot delay the authority. The immutable authority terminal
is published before best-effort filesystem finalization. Terminal replay,
incident, and operational jobs are submitted without blocking, and the
diagnostics writer receives only a short bounded join grace; a wedged filesystem
thread is detached after that deadline. Consequently post-publication writer
counters are not folded back into the already-published terminal snapshot, and
diagnostics failure can never extend authority or application exit indefinitely.
