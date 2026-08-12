# Native Online Runtime

- Status: implemented player-facing native listen application; physical Steam validation pending
- Source: `src/native_online.rs`, `src/native_online_app.rs`
- Feature: `steam-net`
- Decision date: 2026-07-23

`NativeOnlineRuntime` is the native platform owner above
`SteamPlatform<RealSteamBackend>` and `OnlineLobbyCoordinator`.
`NativeOnlineApplication` is the player-facing composition owner above that
runtime. Both are installed as Bevy non-send resources by `build_app`; one
exclusive application-frame system pumps the runtime, stages authenticated
endpoints, starts the listen or remote worker, and projects its latest snapshot.
The canonical simulation world never owns Steam or wall-clock state.

Production orchestration is single-sourced in the internal
`NativeOnlineCore<B, F>`. The Steam platform and lobby coordinator remain
concrete (`SteamPlatform<B>` and `OnlineLobbyCoordinator`); only the transport
factory is replaceable. The shipping specialization is
`RealSteamBackend + RealNativeTransportFactory`, while the public
`NativeOnlineRuntime` API is unchanged. AFCP ticket and manifest frames travel
on the quarantined socket owned by the coordinator; there is no separate
authentication-message port. Field declaration order is intentional: gameplay
endpoints and control state drop before the platform/callback owner.

Without `steam-net`, the same public UI model compiles and reports
`online.unavailable.steam_feature_disabled`. On Web it reports
`online.unavailable.unsupported_platform`. Neither case attempts networking.

## Shipping configuration

There is no default Steam App ID or release label. The only shipping feature
composition is:

```bash
AFC_BUILD_ID=<IMMUTABLE_RELEASE_LABEL> \
AFC_STEAM_APP_ID=<REAL_AFC_APP_ID> \
  cargo build --locked --release --no-default-features --features shipping \
  --bin ffc-prototype
```

This build command is one input to candidate creation, not a complete depot.
Run `scripts/release.py audit-source` before building and use its `stage`,
`verify`, and `archive` commands as specified in
[Native release packaging](release-packaging.md). The protected
[`release-candidate.yml`](../.github/workflows/release-candidate.yml) workflow
applies that contract to Windows, SteamRT4 Linux, and universal macOS from one
commit.

`build.rs` rejects a missing or invalid release label, a missing, zero,
malformed, or Spacewar release App ID, and a release feature composition that
includes developer hot reload or omits native Steam support. It embeds the
validated values as release identity. The App ID contributes to
`AFC_COMPILED_BUILD_ID`, so binaries compiled for different Steam applications
cannot advertise the same multiplayer build identity. A verified depot then
launches through Steam without custom AFC environment variables.

Compatibility digest v2 is an intentional one-time incompatibility with earlier
development binaries. It hashes normalized paths and LF-normalized bytes for every
Rust source, Cargo manifests/lockfile, enabled Cargo features, and `PROFILE`, in
addition to the configured release label and Steam App ID. A debug client compiled
with the real App ID therefore cannot advertise a release client's build identity.
The separate gameplay-content v2 digest remains presentation-independent and now
covers all embedded authored simulation data, including
`arts/champions_court.ron`.

Development builds may select `AFC_STEAM_APP_ID` at runtime. If the binary also
contains a compiled App ID, a different runtime value fails closed. App ID 480
still requires the exact `AFC_STEAM_DEV_SPACEWAR_480=1` opt-in, the opt-in is
invalid for every other ID, and Spacewar is forbidden in release builds.

Before `build_app`, the player executable calls
`SteamAPI_RestartAppIfNecessary` for a valid release identity. A `true` result
returns from `main` immediately; Bevy and the Steam client are not initialized
in the process Steam asked to replace.

`ffc-prototype --release-identity` emits one deterministic JSON object before
initializing Bevy or Steam. It includes the release label, shipping marker,
compiled App ID, product and compatibility identity, the protocol, simulation,
RNG, replay, and snapshot versions, and the gameplay-content hash. Staging
rejects a non-shipping identity, App ID 480, a mismatched label/App ID, or a
non-null pre-upload Steam depot build ID. The same identity must match exactly
across all three native archives.

The automated release workflow generates unsigned candidate archives and
preview-only SteamPipe VDFs; it is not evidence that Valve services were
contacted. Real App/depot variables, Steamworks Partner selection of Steam Linux
Runtime 4 (runtime App ID 4183110), signing/notarization, external upload/branch
promotion, and physical account/device testing remain separate gates.

## Application API

The UI reads `NativeOnlineRuntime::view_model()` and renders its localizable
screen/availability keys. It submits one typed `NativeOnlineCommand` through
`execute(command, monotonic_ms)`:

- `Create` accepts only private or friends-only listen lobbies and includes the
  initial couch-seat/loadout declaration.
- `Join` consumes the exact invite, rich-presence, launch-command, or friends
  join intent plus the local declaration; `DeclineJoin` rejects the prompt.
- `SetLocalDeclaration` and `SetReady` drive the lobby. Invite-overlay requests
  use a separate typed result seam so local overlay unavailability cannot be
  promoted into a fatal command/runtime failure.
- A fresh `Initial` ticket exchange waits for a complete, coherent current
  declaration, but not for its Ready bit. Ready/loadout edits do not replace an
  otherwise valid physical link. Same-match reconnect tickets are likewise not
  readiness-gated.
- `CommitManifest` is listen-owner-only. `AcceptManifest` remains available for
  explicit integrations, while the native owner automatically accepts its own
  committed config and clients automatically accept the validated cross-machine
  bootstrap.
- `ContentLoaded`, `InitialSyncComplete`, `BeginCountdown`, and `MarkFighting`
  advance only their guarded phases.
- `BeginResultConfirmation`, `ConfirmResult`, `Rematch`, `ReturnToLobby`, and
  `LeaveOnline` drive teardown and result flow.
- `ApplyAuthorityDisconnect` is internal worker-to-runtime composition. It accepts
  only a remote-client terminal whose role, match, and publication generation still
  match. The application retains the first valid payload and derives its available
  actions solely from the stable retry disposition; raw detail/tick fields are not
  rendered. `Retry` remains reserved for failures explicitly classified as Retry
  and is not an alias for reconnect.
- `QuiesceAdmission` is the first graceful-shutdown command. It atomically fences
  transport construction, listener/pending admission, ticket exchange, AFCP
  control outboxes, reconnect markers, pending manifest handoff, and new
  endpoint delivery for the current match. It intentionally retains established
  worker-owned endpoints so typed terminals and their ACKs can drain.
- `MarkAuthorityTerminalDrained` is the listen-side cleanup handoff. The
  application must resolve the worker's exact attached
  `(peer_id, SteamUserId, AuthorityConnectionId)` generation to its admitted
  `SteamConnectionId`; the coordinator treats a stale mapping as a benign no-op.
  This command is cleanup-only and never authors a gameplay terminal.

One-shot work is available through `poll_event()`. `take_endpoint()` returns a
`NativeOnlineEndpoint`, keeping `PeerId`, reconnect intent, authenticated Steam
admission, and `SteamDatagramEndpoint` atomic. This prevents the application
from correlating separate queues incorrectly.

`transport_retirement_pending()` is the runtime's native-lifecycle seam. It stays
true while the coordinator owns any old match transport whose outbound drain or
delayed Steam-auth cleanup has not reached a terminal outcome. A composition
performing graceful leave or process shutdown must keep the runtime/platform
owner alive and pumping while this is true, subject to its own bounded outer
shutdown deadline; dropping the runtime is the emergency path.
`admission_is_quiesced()` exposes the independent admission fence; shutdown must
raise it before asking the authority worker to begin its drain.

The manifest transaction freezes a fixed-capacity, ticket-free
`CommittedAuthenticatedRoster`. The application consumes it through
`committed_authenticated_roster()` only atomically with the exact admitted
endpoints after activation completes and the coordinator enters Loading. Listen
authority startup is:

```text
let peers = runtime.committed_authenticated_roster();
let config = runtime.match_config();
let roster = ListenAuthenticatedRoster::new(config, host, peers.iter());
let online_match = ListenOnlineMatch::spawn(config.clone(), roster, ...);
```

No authentication ticket bytes or arbitrary Steam diagnostic strings reach the
screen model, event stream, logs, or authority attach payload.

## First-release trust gate

The native menu exposes only `Create Private` and `Create Friends`. Public,
ranked, trusted, and dedicated choices are not rendered as player-facing
capabilities. Defensive typed actions for an injected trusted/dedicated request
still fail closed and issue no runtime command.

The restriction is enforced below the menu as well:

1. create and entered-lobby metadata must be private/friends-only and listen;
2. manifest options and AFCP manifest proposals must be listen and untrusted;
3. the frozen metadata contract must match authority, visibility, rules, arena,
   and seat capacity;
4. the same contract and manifest are validated again before countdown.

These checks are release policy, not a fallback. An unsupported request never
silently becomes a casual listen match.

## UI-independent screen model

`NativeOnlineViewModel` projects the coordinator state into these visual routes:

```text
Unavailable
OnlineMenu -> CreatingLobby | JoinPrompt -> JoiningLobby
Lobby -> Connecting -> Authenticating -> ManifestAgreement
Loading -> Ready -> Countdown -> Fighting
Fighting -> Reconnecting -> Fighting
Fighting -> ConfirmingResult -> Results
Results -> owner-authored epoch -> Lobby (rematch/return) | OnlineMenu
any guarded failure -> Error
```

The model includes action availability, lobby/role/member counts, couch seat
count and ready state, worst peer quality, input-delay calibration state and
selected immutable delay, relay status, actual countdown tick,
confirmed/no-contest outcome, and a stable `OnlineFailure`. It covers the data
needed by the Online entry, private/friends creation, invite/launch prompt,
couch loadout lobby, quality indicator, loading/countdown, reconnect overlay,
results/rematch, return, and error screens without depending on Bevy UI types.

## Cross-machine pre-game control

Pre-game setup and gameplay share one explicit Steam Networking Sockets P2P
connection. The connection opens immediately after lobby entry—independently of
Ready—and remains quarantined until authentication and manifest agreement finish.
Clients connect only to the Steam-confirmed lobby owner; the authority accepts
only current lobby members, preserving the authority-star topology.

Lobby schema 4 uses the bounded AFCP version 2 control protocol. It carries
`LinkHello`, the direct `AuthTicket` / `AuthAccepted` exchange, roster-auth
messages, the manifest/activation transaction, and `Abort` / `SetupCancel`.
Each envelope is at most 1,200 bytes and binds the lobby, outer-hop sender and
recipient, physical connection generation, sequence, acknowledgement generation,
and acknowledgement. Reliable submission is not application delivery: a
piggyback acknowledgement and the inbound sequence are committed only when the
runtime semantically accepts the decoded message through its ingress token.
Invalid or future ACKs cannot mutate the outbox. Same-generation duplicates are
idempotent, `LinkHello` precedes standalone ACK traffic, and a replacement
generation receives fresh one-use tickets.

The physical connection follows this monotonic security lifecycle:

```text
Connecting -> ControlReady -> Authenticating -> Secure
           -> ManifestAgreement -> GameplayReceiveArmed -> GameplayReady
```

`ControlReady` grants no seat or gameplay capability. Promotion to `Secure`
requires both successful validation of the remote ticket and receipt of
`AuthAccepted` for the local ticket. Ticket callback order is irrelevant: ready
tickets remain retained until the matching physical connection is control-ready.
Ticket buffers are redacted from `Debug` and explicitly zeroized.

The physical socket topology remains a star, but account authentication covers
the full roster. Once direct authority/client links are secure, the authority
sends `RosterPrepare` for one account-set hash and non-zero auth epoch. Clients
authenticate every other client with recipient-bound one-use tickets carried as
`RoutedAuthTicket` frames through the authority; receipts return as
`RoutedAuthAccepted`. The authority validates each outer hop and forwards the
secret-bearing payload only to its declared logical recipient. It never grants a
seat from a forwarded ticket. `RosterAccepted` and `RosterAuthComplete` make the
epoch globally complete only after all participants have validated all `N-1`
remote accounts. Roster changes retire the epoch, end those auth sessions, cancel
issued tickets, and require a new coherent epoch without replacing otherwise
valid physical links.

Start uses a transaction-bound multi-barrier agreement. `ManifestPrepare` freezes
the manifest hash and exact participant-to-connection-generation set;
`ManifestAccepted` cannot substitute a later socket. After every acceptance, the
authority sends `ManifestCommit` and waits for every `ManifestCommitAccepted`.
Only then does it commit locally, arm those exact sockets for hidden receive-only
AFCN buffering, and send `GameplayActivate`. A client commits and arms before it
replies `GameplayActivated`, but it does not expose an endpoint yet. After every
activation receipt, the authority sends a reliable `GameplayActivated` final
release to every client and promotes its own endpoints; each client promotes only
after receiving that release. AFCN received before receive-arming is
malformed; AFCN received while armed is buffered but cannot be exposed or sent.
An `Abort`, `SetupCancel`, rejection, or the fixed 10-second authority activation
deadline returns both sides to Lobby and rolls `ManifestAgreement` or
`GameplayReceiveArmed` back to `Secure` before the final-release barrier; no
uncommitted client worker is created. Final release is irrevocable and retained
for reliable retransmission rather than being canceled after partial delivery.
Known retired-transaction frames are semantically ACKed and ignored so reliable
ordered sequences do not acquire gaps.

`NativeOnlineViewModel` separately reports declared readiness, connected and
secure remote-link counts, required remote-link count, verified and required
remote-account counts, relay/certificate readiness, setup stage, and the first
Start blocker. Its `all_members_ready` input covers coherent Ready declarations
only. Link requirements are role-aware: the listen authority needs
`N-1`, each client needs one, and every participant needs `N-1` verified remote
accounts. The lobby therefore shows actionable states such as “Preparing Steam
network”, “Connecting”, “Validating account”, and “Waiting for manifest” instead
of one opaque aggregate readiness line.

Relay access and Steam Networking Sockets authentication initialization begin
together. Preparation and each connection/authentication setup are bounded to 15
seconds. Entering Online may retry a terminal initialization failure. An early
transient link failure gets one new generation after 500 ms only if at least five
seconds remain. If a client-originated control link exhausts that automatic retry,
only the client acts on the actionable Retry and opens the replacement generation;
the authority keeps a passive attributed failure until the inbound replacement is
secure. Lobby membership, the invitation, and unrelated star links remain intact.

An in-lobby Steam backend disconnect instead starts a 10-second reconnect grace.
The platform keeps the active lobby, issued tickets, authentication sessions,
secure sockets, and established gameplay endpoints alive. The coordinator keeps
the transport pumping but disables incoming admission and prevents new ticket,
authentication, declaration, or manifest capability from advancing. It defers
bounded auth/setup callbacks and pauses the active flow, network preparation,
authentication-lease, and activation deadlines for exactly the outage duration.
On reconnect it revalidates local membership, immutable lobby metadata, and the
current owner before releasing deferred work. Missing membership, incompatible
metadata, or grace expiry produces a safe lobby exit; the UI reports Steam network
preparation as retrying during the grace rather than offering a competing manual
Retry.

Every physical generation keeps a 64-event privacy-safe trace containing only
ordinal, generation, setup phase, elapsed milliseconds, bounded relay/auth
availability, stable AFC result code, and the numeric native end reason. A
pre-game failure is stored under the normal diagnostics root in a dedicated
64 KiB-per-file, 16-file/1 MiB-total archive. These records have no fields for
Steam IDs, addresses, persona names, ticket or payload bytes, or Valve's
free-form diagnostic text. Peer isolation closes with an explicit stable terminal
classification: malformed AFCP traffic is `413`, while permanent Steam
ticket/account rejection is `415`; neither is mislabeled as a requested close.
Isolation drains and persists the exact completed trace before runtime handoff
cleanup. A failed generation retired by automatic `ControlRetrying` is also
drained and persisted even when its replacement later succeeds. Filenames include
a stable content fingerprint, so two distinct same-generation/result-shaped
failures coexist while a byte-identical repeat remains idempotent.

## Player-facing application integration

The native application integration is installed in `build_app`:

1. the user-mode main menu exposes a separate Online route;
2. `setup_native_online_ui` builds panels and actions for private/friends create,
   invite/join, couch seats and loadouts, ready/start, calibration/quality, countdown,
   reconnect, results, rematch, return and errors;
3. `drive_native_online_application` is the sole frame pump and switches the
   render world to `ExternalProjection` before an online simulation can advance;
4. `NativeOnlineApplication` consumes the committed roster/config and admitted
   endpoint atomically, then starts `ListenOnlineMatch` or `RemoteOnlineClient`;
5. content readiness advances both worker and coordinator gates; local inputs are
   sampled by couch ordinal, mapped to owned protocol seats, and submitted at the
   fixed boundary; and
6. result, teardown, host-loss and reconnect observations feed the bounded screen
   model and stable localized failure keys.

For every listen-authority attach event, the composition must retain the exact
worker connection generation together with the endpoint's Steam connection ID.
Only an exact `TerminalDrained` event may consume that mapping and submit
`MarkAuthorityTerminalDrained`. Keeping the mapping until either terminal
publication or exact native close makes callback order irrelevant and prevents an
old worker generation from cleaning up its same-identity replacement.

Both disconnect and authentication-rejection handoffs retain the admitted
`SteamConnectionId`. Runtime mapping cleanup and application detach/revoke are
exact-generation operations. A rejection with `None` targets only a local or
pre-attach mapping; it cannot clear an already attached replacement. A delayed
`Some(old_connection)` event is observable for diagnostics but is nondestructive
when the active mapping names `new_connection`.

Steam connection-quality samples are forwarded into a gameplay client worker only
for a remote-client session and only when the sampled Steam user is that worker's
installed owner endpoint. A listen owner's local loopback client never consumes a
remote peer's Steam RTT/loss as if it were the host's own authority link.

Native controls support pointer/keyboard and the bundled Steam Input menu/gameplay
action sets. Controller ordinals remain stable for online couch seats, menu actions
are edge-latched across screen transitions, and gameplay actions enter the same
tick-owned input accumulator as keyboard input. See
[Steam Input integration](steam-input.md) for the action manifest, shipping asset
placement, automated coverage, and still-required physical-device acceptance.

The application lifecycle fixture starts a listen owner and remote application
over authenticated fake Steam endpoints, replaces the remote connection with an
authenticated reconnect endpoint, applies the fresh authoritative snapshot, and
resumes the existing fight. It proves that this replacement re-arms only the
coordinator's `InitialSyncComplete` gate: manifest acceptance, content loading,
countdown selection, and new-match transitions are not replayed. The fixture then
completes an authored stock-rules match, confirms the same result on both sides,
then pumps both Completed endpoint owners beyond the 50 ms transport quiet window.
The listen owner acts first and retires its gameplay endpoint; the remote remains
in confirmed Results beyond another quiet window without a transport failure,
then rematches, starts a second match, returns to the lobby, and joins every
worker during final teardown. Coordinator fixtures independently cover
client-first deferral and owner-first versioned reset/ack ordering. These are
automated fake-transport/application-boundary results, not yet cross-machine Steam
composition evidence. Additional fixtures prove that recoverable connection
failures retain the reconnectable client while fatal terminal-worker and
bounded-capacity failures synchronously join the active worker, clear staged
endpoints and authority retries, and release the projected render world before the
Error screen is visible. `pump_frame` returns its fatal error to the application;
that frame skips application pumping, Steam Input setup, and projection, and
releases render/input ownership before fixed-step gameplay can run. Fatal transport
pump failures expose a local `ReturnToMenu` path rather than a reconnect action;
ordinary attributed connection-close events still use reconnect. Error-screen
actions are projected from executable coordinator transitions only. Return and
Retry perform local worker/handoff cleanup even when best-effort native leave
fails, so a broken platform backend cannot trap the user in an active local
session.

A production-orchestration fixture constructs independent `NativeOnlineCore`
instances with independent fake Steam backends. A minimal
mirror fabric propagates only owner lobby state and per-user membership and
declarations, never a shared global backend or two simultaneously held backend
locks. All ticket, roster-auth, manifest, and activation frames use AFCP over one
shared `FakeSteamTransportNetwork`; no parallel authentication-message port is
involved. Pair fixtures prove create/join, direct link authentication, semantic
ACK, one shared physical connection generation, manifest abort, identical
manifest/config/frozen roster, rematch orderings, a new `MatchId` and connection,
completed retirement, and stale-generation immunity. The four-core fixture proves
that four Steam accounts use three star links, every participant verifies the
other three accounts through direct or routed recipient-bound tickets, Ready
declarations may arrive in any order, and exact participant generations survive
Prepare/Accepted, Commit/CommitAccepted, receive arming, and
Activate/Activated before endpoint handoff.

One combined fixture implements the production `NativeOnlineRuntimePort` for
two independent fake-Steam `NativeOnlineCore` owners, then drives both through
`NativeOnlineApplication` into the real listen/remote workers. It covers
application-authored create, invite join, client-first Ready, Start, AFCP
manifest/activation, endpoint handoff, countdown/fighting, client/owner return,
leave, and zero surviving socket/ticket/auth resources. It never assigns
`all_members_ready`, committed config/roster, authenticated mappings, or admitted
endpoints. The real `NativeOnlineRuntime` remains intentionally specialized to
the sole Steam client owner; the test-only adapter crosses that privacy boundary
without adding a second production owner.

## Remaining release gates

Release acceptance still requires licensed Steam accounts on separate machines
to validate private/friends create and invite, launch join, direct and routed
AFCP ticket exchange, full-roster account validation, immutable manifest and
activation barriers, SDR endpoint admission, couch seats, countdown,
suspend/disconnect/reconnect, host loss/no-contest, rematch, return, and clean
shutdown. Include a 3–4-account run for the socket-star/routed-auth boundary.
The App ID 480 physical matrix remains pending, development-only, and never
shipping evidence. Record results against the exact sealed manifest and archive
hashes in [Steam release acceptance](steam-release-acceptance.md).

The separate `afc-dedicated` all-bot executable is deployment/test-only. Running
it does not enable or validate hosted Steam SDR, player admission, ranked play,
or trusted results.
