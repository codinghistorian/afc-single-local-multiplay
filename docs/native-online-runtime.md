# Native Online Runtime

- Status: implemented player-facing native listen application; two-machine Steam validation pending
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
`NativeOnlineCore<B, S, F>`. The Steam platform and lobby coordinator remain
concrete (`SteamPlatform<B>` and `OnlineLobbyCoordinator`); only the
non-generic authentication-signal port and transport factory are replaceable.
The shipping specialization is
`RealSteamBackend + SteamAuthSignalChannel + RealNativeTransportFactory`, while
the public `NativeOnlineRuntime` API is unchanged. No thread-safety, cloning, or
debug-printing capability is required of either seam, and ticket/manifest
signals cross it by value. Field declaration order is intentional: gameplay
endpoints drop before signaling and coordinator state, and all of those drop
before the platform/callback owner.

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
- A fresh `Initial` ticket exchange waits for the current declaration to be
  ready. This preserves a deterministic Lobby editing/readiness window after
  create/join and after an owner-authored rematch epoch. Same-match reconnect
  tickets are not readiness-gated.
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
  transport construction, listener/pending admission, ticket exchange, native
  authentication signals, reconnect markers, pending manifest handoff, and new
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

At manifest commit, `committed_authenticated_roster()` exposes a fixed-capacity,
ticket-free `CommittedAuthenticatedRoster`. Listen authority startup is:

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
2. manifest options and bootstrap manifests must be listen and untrusted;
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

## Cross-machine pre-game signaling

Authentication cannot use Steam lobby chat, and the gameplay transport cannot
carry packets before authenticated admission. The runtime therefore owns a
dedicated reliable `ISteamNetworkingMessages` channel for exactly two bounded
pre-game messages:

1. Steam authentication ticket, at most 1024 bytes;
2. owner-to-client canonical `StartMessage::Manifest` AFC wire packet.

Incoming message sessions are accepted only for Steam identities in the union of
the current bounded lobby roster and coordinator-authorized committed peer
leases. This lets an exact same-match reconnect survive callback-order gaps
without opening admission to an arbitrary nonmember.

Authentication tickets use envelope version 2 and a 62-byte fixed header:
magic/version/kind/purpose, active lobby, actual Steam sender, recipient,
sender `PeerId`, non-zero owner and sender declaration revisions, 16 `MatchId`
bytes, and exact ticket length. Initial tickets require an all-zero `MatchId`;
Reconnect tickets require the exact current non-zero `MatchId`. The sender Steam
ID is still attributed independently by `ISteamNetworkingMessages`.

Ingress compares the envelope with immutable coordinator leases. A well-formed
older revision or old/wrong-match reconnect is discarded as benign stale work,
without authentication, quarantine, or a user-visible failure. A current-epoch
wrong peer, malformed envelope, wrong recipient/lobby, or unattributed nonmember
is isolated fail closed. First-match Initial authentication requires a live
coherent roster entry. A post-result Initial may use the prior immutable
Steam-user/peer lease plus a higher sender revision when its roster callback is
late.

Tickets are issued automatically in both directions after complete member
metadata exists. Issue freezes the exact sender, recipient, revision, purpose,
owner epoch, lobby, and match scope. They are transmitted only after an
`AuthTicketReady` callback still matches that complete lease; an inverted stale
callback is cancelled silently, while a fresh lease may proceed. The secret bytes
are moved once into the signal, consumed by `begin_peer_authentication`, redacted
from every `Debug` implementation, and explicitly zeroized before owned buffers
are released. The Steam handle remains available for cancellation; gameplay
`SteamTransport` admission is unchanged and still waits for the platform
validation callback and App ID ownership result. Reliable redelivery of an exact
lobby/sender/peer/revision/epoch/purpose ticket reuses the pending or
already-consumed authentication admission; it cannot create another Steam auth
session, consume a second capability, or turn a valid replay into a host-global
runtime failure.

After owner commit, the runtime sends the canonical manifest to every
authenticated remote lobby member. A client requires the actual sender to be the
current Steam-confirmed lobby owner for that between-match session, validates
compatibility and the canonical manifest hash through the AFC codec, reconstructs
`HeadlessMatchConfig`, and calls `accept_manifest`. Acceptance reconstructs the
full canonical roster from the coherent Steam declaration snapshot, including
third-party members the client does not authenticate directly, and requires exact
manifest equality. A valid early arrival is staged until the gameplay endpoint
reaches manifest agreement. A coherent snapshot that is still staging keeps the
manifest pending and is retried after metadata callbacks; it is not quarantined as
malicious traffic. Exact duplicates are idempotent; conflicting accepted or staged
manifests and coherent roster mismatches fail closed. The later AFC gameplay
handshake must repeat and exact-match this same manifest because the remote worker
is spawned from the bootstrapped config.

This provisional channel is not a gameplay, snapshot, input, result, or chat data
plane. Per-pump receive work is capped at 16 messages; overflow, malformed data,
identity mismatch, and channel failure become sanitized fatal online failures.

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

A separate production-orchestration fixture constructs two
`NativeOnlineCore` instances with independent fake Steam backends. A minimal
mirror fabric propagates only owner lobby state and per-user membership and
declarations, never a shared global backend or two simultaneously held backend
locks. An exact-generation, source-attributed bounded fake auth bus connects the
cores, and both transport factories use one shared
`FakeSteamTransportNetwork`. Split tests prove create/join, bidirectional Steam
auth, one shared physical connection generation, identical manifest/config and
frozen authenticated roster, both client-first and owner-first rematch intent,
revision 2 declarations, a new `MatchId`, a new physical connection, completed
old-transport retirement, and immunity of the replacement mapping to an old
generation terminal-cleanup handoff.

That core fixture ends at the existing application handoff
(`NativeOnlineEndpoint` plus committed config/roster). The application lifecycle
fixture exercises the real listen/remote workers above the same handoff using
its scripted runtime port. Combining those two fixture owners into one test is
additional end-to-end test composition, not a missing shipping call site; the
real `NativeOnlineRuntime` remains intentionally specialized to the sole Steam
client owner.

## Remaining release gates

Release acceptance still requires two licensed Steam accounts on separate
machines to validate private/friends create and invite, launch join, the
NetworkingMessages ticket/manifest exchange, SDR endpoint admission, couch
seats, countdown, suspend/disconnect/reconnect, host loss/no-contest, rematch,
return, and clean shutdown. App ID 480 is development-only and is not shipping
evidence. Record those results against the exact sealed manifest and archive
hashes in [Steam release acceptance](steam-release-acceptance.md).

The separate `afc-dedicated` all-bot executable is deployment/test-only. Running
it does not enable or validate hosted Steam SDR, player admission, ranked play,
or trusted results.
