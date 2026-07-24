# Online Lobby and Application Coordinator

- Status: implemented and native-application wired; two-account Steam validation pending
- Source: `src/online_lobby.rs`
- Decision date: 2026-07-23

`OnlineLobbyCoordinator` is the native, UI-independent owner of the online
application flow. It joins the Steam platform boundary, authenticated roster,
Steam gameplay transport, and immutable match bootstrap without putting Steam,
socket, asset, or UI types into deterministic gameplay.

## Ownership and pump order

Construct one coordinator for the exact `SteamPlatform` local identity. On each
native application iteration, call only:

```text
OnlineLobbyCoordinator::pump(&mut SteamPlatform, monotonic_ms)
```

The coordinator enforces this order internally:

1. pump the sole Steam callback owner;
2. drain lobby, invite, metadata, ticket, and authentication events;
3. pump old match transports in outbound-only retirement and complete any
   deferred platform leave whose retirements are terminal;
4. reconcile authenticated member declarations into `OnlineRoster`;
5. refresh admission policy and pump the installed active `SteamTransport`;
6. drain pending/connected/closed active-transport events;
7. sample bounded integer network-quality data and enforce only transitions into
   sustained `Reject`;
8. apply wall-clock operation timeouts.

The coordinator is not cloneable. Gameplay endpoints are moved out exactly once
with `take_endpoint`, while the coordinator retains and pumps their transport
connections. Match teardown moves an active transport into a fixed queue of at
most four retired generations, leaving the active slot available for a new
between-match transport. Queue exhaustion fails closed with
`RetiringTransportCapacity`; it never turns teardown into unbounded retention.

Retirement is logical before it is physical. Old endpoints disappear from the
active roster and cannot authenticate, reconnect, receive, admit, or publish
events, while their already-queued outbound datagrams retain a bounded final send
opportunity. The coordinator holds the exact issued tickets and authenticated
Steam users with that retired generation until its transport reports `Complete`,
`TimedOut`, or `Faulted`. Only then does it cancel the tickets, end the
authentication sessions, and remove the retiring bindings. Leaving a lobby is
similarly deferred until this cleanup point so `SteamPlatform::leave_lobby`
cannot erase auth state underneath the drain.
`OnlineTransportRetirementMetrics` counts `started`, `completed`, `timed_out`,
and `faulted`, plus a queue `high_water`; the transport itself supplies the
separate 300 ms absolute cap.

`quiesce_admission(&mut SteamPlatform)` is the atomic shutdown fence. It stops the
listen socket, rejects recorded pending inbound links, removes queued
transport/ticket/authentication/endpoint capability events, cancels issued
tickets, and ends authentication that has not attached an established link.
Established links and already-moved endpoints remain owned and pumpable for their
bounded terminal/ACK drain. Ticket-ready/authentication callbacks that race the
fence are consumed without publishing new work. The fence is idempotent and is
cleared only by a fresh create/join or completed owner-authored return-to-lobby
epoch.

## UI command and status seam

Native menus call typed methods for create, join, invite overlay, loadout change,
ready/unready, ticket issue/authentication, manifest commit/accept, loading and
sync completion, countdown, result confirmation, rematch, return, and leave.
`OnlineLobbyStatus` is a copyable screen-model snapshot. `OnlineLobbyEvent` is a
bounded event stream for transitions and one-shot work such as:

- showing an invite/launch join prompt;
- constructing the requested listen-authority or client Steam transport;
- sending a Steam-ready authentication ticket through bounded pre-game signaling;
- attaching an admitted endpoint to the authority hub or remote client worker;
- showing quality changes and localizable `OnlineFailure` values;
- dropping gameplay endpoints during teardown;
- presenting confirmed or host-loss/no-contest results.

Disconnect and rejection events are generation-bearing. `PeerDisconnected`
contains the exact `SteamConnectionId`. `PeerAuthenticationRejected` contains
`Some(SteamConnectionId)` when revoking an attached link and `None` only before
attach or for the local identity. Downstream cleanup must compare that value with
the currently attached generation before clearing a binding.

The status also exposes sanitized Steam relay availability and the worst current
peer-quality snapshot; it never exposes Steam's unbounded diagnostic strings.
`total_seats`, `seat_capacity`, and effective joinability come from the platform's
coherent Steam-member projection, not the authenticated gameplay roster. This lets
the UI disable an aggregate-capacity edit before authentication has finished and
prevents an unauthenticated declaration from disappearing from capacity accounting.

Lobby loadout edits are application-level transactions too: the UI mutates a copy
of its couch-seat editor, asks the platform to publish the new declaration, and
commits the editor copy only after publication succeeds. A capacity race therefore
does not consume a revision or leave the displayed seats different from Steam
metadata.

Lobby chat is not a ticket, manifest, input, snapshot, or result data plane. The
ready auth-ticket event is the handoff to the product's bounded pre-game signaling
message. Ticket issue freezes one exact `AuthTicketLease`: handle, recipient and
recipient declaration revision, sender Steam user/peer/revision, active lobby,
purpose, owner revision, and optional committed `MatchId`. A ready or rejected
callback is applied only to that exact lease. If metadata, match, purpose, or
declaration state changed before the callback, the coordinator cancels and
consumes the stale callback without publishing work. `take_ready_auth_ticket`
requires the same full lease and moves the ticket bytes exactly once; the Steam
platform retains only the handle needed for later cancellation.

Manifest commit freezes an `AuthPeerLease { user, peer_id, revision }` for every
authenticated identity. Reconnect authorization comes from these committed
leases and the current non-zero `MatchId`, not from a transient Steam roster
snapshot. An exact committed peer may therefore reconnect while its member
callback is temporarily absent. Initial authentication for the first match still
requires live coherent member metadata. After a confirmed result, an Initial
signal may advance a previously committed identity to a higher declaration
revision even if that member callback arrives later. A well-formed older
revision, older owner epoch, or different old `MatchId` is a benign stale no-op;
malformed envelopes and a current-epoch peer mismatch remain peer-scoped hostile
input.

## State and timeout policy

`OnlineFlowMachine` is the deterministic transition core. It rejects skipped
gates and time regression, and assigns an absolute monotonic deadline on entry to
every asynchronous phase:

```text
Offline/Menu
  -> InvitePending | CreatingLobby | JoiningLobby
  -> Lobby
  -> Connecting
  -> Authenticating
  -> ManifestAgreement
  -> Loading
  -> InitialSync
  -> Ready
  -> Countdown(actual_start_tick)
  -> Fighting
  -> ConfirmingResult
  -> Results
  -> ReturningToLobby | Lobby(rematch)
```

Reconnect is an explicit branch from countdown or fighting. The coordinator
records a typed resume target, so a countdown disconnect returns through initial
sync to the same `Countdown(actual_start_tick)` while a combat disconnect returns
to `Fighting`. A client reauthenticates the same Steam identity and
authority-assigned peer ID and establishes a new connection. The immutable
manifest, seat ownership, and selected countdown boundary are retained. The
listen socket remains auth-gated and open during the match so reconnect can work
even though Steam lobby joinability is closed to new peers.

Timeouts project to stable, localizable `OnlineFailure` codes. Platform and
transport implementation errors are similarly projected without exposing remote
strings, auth tickets, Steam diagnostics, or raw backend data to UI.

## Match-start invariants

The listen owner is the only party that can call `commit_manifest`. Commit fails
unless all of the following are true:

- every Steam member has complete ready, seat-count, and loadout metadata;
- every declaration is bound to one authenticated Steam identity and unique peer;
- every remote member has a connected, admitted gameplay endpoint;
- no connected peer is in sustained network-quality `Reject` state;
- authority, trust, arena, and rules agree with the lobby contract;
- listen play is untrusted;
- the canonical roster can produce one valid headless match configuration.

Before commit, every authenticated remote connection has its own resettable
32-sample RTT calibrator. Twenty valid samples make that connection eligible;
unknown ping values are ignored. The listen authority chooses the worst
nearest-rank p95, converts it to an immutable 2–6 tick input delay, and fails the
preflight if calibration is incomplete, the proposed delay differs, or the
required half-RTT-plus-delay exceeds the 12-tick rollback limit. A lobby with no
remote peer deterministically selects two ticks.

Commit first closes Steam lobby joinability/rich-presence admission, then builds
one immutable manifest containing that selected delay. A client authenticates
only itself and the listen owner;
it does not invent third-party authentication. To validate a three- or four-member
bootstrap, it reconstructs a temporary full roster from the platform's coherent
member declarations, pins every known user/peer mapping, and matches remaining
peer groups by exact ordered couch-seat signature. Identical signatures are
interchangeable. It rebuilds the canonical configuration at tick zero with the
received options and requires exact manifest equality. Pending/staging member
metadata defers acceptance; a coherent omission, mutation, reassignment, or
noncanonical manifest fails closed. The manifest's `agreed_start_tick` is an
earliest proposal; `begin_countdown` accepts the later authority-selected start
tick only after readiness.

Member metadata rejection is attributed rather than promoted automatically into a
lobby-wide failure. A listen authority isolates only the rejected remote. A client
fails if its authority's declaration is rejected. Either role fails locally, with
a leave/menu recovery, if its own declaration loses continuity or capacity.
An unrelated third-party rejection observed by a client only rebuilds that member
as pending; the client has no authority to revoke another peer's authentication.

## Failure and recovery behavior

- A remote disconnect on a listen authority does not stop the match; it emits a
  reclaim event and preserves the immutable match contract.
- A committed Steam-member departure closes every attributed pending/connected
  transport, ticket, authentication session, and endpoint before publishing the
  disconnect and roster barrier. Its fixed Steam-user/peer lease and quality
  history remain reserved for same-identity reconnect. Repeated departure
  callbacks are idempotent, and a queued close callback can mutate a binding only
  when its connection ID still matches the recorded current or pending link.
- Incoming P2P attempts from nonmembers are rejected before consuming a bounded
  coordinator pending slot. After manifest commit, even a current lobby member
  must also hold a retained committed identity lease.
- A client authority-connection loss enters reconnect while Steam still reports
  the same lobby authority.
- An exact authenticated typed authority terminal is applied before its expected
  physical close and is first-wins for that match. `ReconnectAllowed` retains the
  remote client and enters reconnect; `ReturnToLobby` and `Fatal` expose only their
  respective lobby/menu action; `MatchEndedNoContest` tears down match transport,
  emits one no-contest result, and offers Return-to-Lobby. A later generic close
  may complete cleanup but cannot replace the typed code or retry disposition.
- Listen-authority terminal cleanup is keyed to one exact physical generation,
  not merely a Steam user or protocol peer. The application retains the mapping
  established when an endpoint is attached:
  `(peer_id, SteamUserId, AuthorityConnectionId) -> SteamConnectionId`. When that
  exact authority generation publishes `TerminalDrained`, it calls
  `mark_authority_terminal_drained` with the mapped Steam connection. A wrong
  peer, user, role, or stale generation is a benign `false` and cannot revoke a
  replacement. If the native close callback wins the race, the coordinator
  retains that exact close fact for one coordinator turn so the matching marker
  can consume it; otherwise the ordinary disconnect path resumes on the next
  turn.
- `ReconnectAllowed` additionally grants that exact live Steam connection a
  one-shot replacement capability before moving it to `retiring_connection`.
  The binding ends the old Steam authentication, retains the old physical ID for
  outbound drain, and accepts only a freshly authenticated `Reconnect` generation
  into the separate active connection field. Non-reconnect terminal dispositions
  never grant overlap. A delayed old close clears only `retiring_connection`; it
  cannot end replacement auth or clear the new admission/endpoint.
- Loss of the listen authority during countdown or combat ends the match as
  no-contest, drops match-scoped authentication and gameplay endpoints, retains
  the same Steam lobby after Steam transfers ownership, and cannot grant a
  confirmed result. No replacement transport is requested from Results.
- Once a result is confirmed, later authority departure tears down only transport
  and authentication. `Confirmed` is monotonic: it is not replaced by
  `NoContestHostLost`, and no second result or failure event is emitted.
- Authority loss before countdown/combat begins is a lobby failure, not a match
  result. The retained lobby exposes a recoverable Return-to-Lobby action with the
  Steam-selected owner and corresponding client/listen role already installed.
- A confirmed result returns to the same lobby through an owner-authored,
  versioned between-match epoch. A non-owner Rematch or Return-to-Lobby action is
  retained as one bounded intent while that client stays in Results; it cannot
  tear down the old endpoint or originate Initial authentication. The listen owner
  advances its Steam member-declaration revision and republishes itself unready.
  Every client follows only a coherent newer unready owner declaration, advances
  its own revision, clears the old match, and requests the next transport.
- The owner does not issue a fresh Initial ticket to an existing client until it
  observes that client's declaration revision advance beyond the committed-match
  floor. This acknowledgement prevents either client-first or owner-first ordering
  from racing an Initial ticket into a peer that is still in Results. Rematch and
  Return share this safe reset; the early client intent changes only the local
  `ReturnedToLobby { rematch }` presentation event.
- A gameplay-link close that arrives after a confirmed Results state but before
  the owner epoch is a benign physical retirement. It does not enter reconnect,
  replace Confirmed, or expose a failure. ConfirmingResult and Fighting retain
  their existing fail-closed/reconnect/no-contest behavior.
- Host-loss Return-to-Lobby uses the same reset path. All survivors reauthenticate
  and re-ready; only the selected owner requests a listen transport and may commit
  the next match. This is between-match replacement, not host migration.
- Warning and degraded quality remain diagnostic/UI states. On the hysteretic
  transition into `Reject`, a listen owner closes only that attributable remote
  link, ends its match-scoped authentication, removes its endpoint, and blocks
  same-match ticket/reconnect admission. Other peers and the authority remain in
  Fighting. Return-to-Lobby clears this match-scoped block.
- A client that locally rejects its owner link closes gameplay and enters a stable
  `NetworkQualityRejected` failure with Return-to-Lobby recovery. It never enters
  the reconnect loop for the same rejected route.
- If the original lobby no longer exists, return resolves to the offline menu.

## Deterministic fake coverage

Focused tests use both `FakeSteamBackend` and `FakeSteamTransportNetwork`. They
cover guarded transitions/deadlines, create and metadata publication, invite and
ticket readiness, two-peer authentication, explicit inbound admission, rogue
pre-capacity rejection, stale-close protection, exact three-member bootstrap,
pending-declaration deferral, endpoint handoff, manifest commit, quality
projection, countdown/fighting same-identity reconnect, listen-host no-contest
with retained lobby/owner transfer, monotonic confirmed results, pre-match
owner-loss recovery, fresh successor authority commit, quality hysteresis plus
bad-peer-only isolation/client terminal rejection, rematch reset, client-first
deferred Results intent, owner-first reset, benign post-result close ordering, and
per-member revision acknowledgement before fresh Initial authentication. They
also cover final-datagram delivery across lobby leave, delayed auth/lobby cleanup,
exact retirement metrics, and both orderings of the generation-safe
`TerminalDrained`/native-close race.

Native release acceptance still requires two licensed Steam accounts on separate
machines for invite, launch-command join, auth ticket exchange, SDR routing,
disconnect/reclaim, owner loss, overlay, suspend/resume, and clean shutdown.
