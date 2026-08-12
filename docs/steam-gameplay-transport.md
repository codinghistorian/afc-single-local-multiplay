# Steam Gameplay Transport

- Status: **implemented production listen path; physical Steam validation pending**
- Source: `src/steam_transport.rs`
- Feature: `steam-net`
- Binding: exact `steamworks = 0.12.2`
- Mode: listen-authority Steam Networking Sockets P2P, with SDR initialized

## Ownership and pump order

`SteamPlatform<RealSteamBackend>` remains the only owner of
`steamworks::Client::run_callbacks`. `SteamTransport::from_steam_platform` takes a
shared client lease, not a second callback pump. The application must execute this
order frequently on the native platform owner:

1. `SteamPlatform::pump(monotonic_ms)`
2. drain platform authentication/lobby events and supply completed admissions
3. pump every coordinator-owned retiring transport
4. `SteamTransport::pump(monotonic_ms)` for the active match, when present
5. pump each `NetworkRuntime<SteamDatagramEndpoint>`

Dropping the platform callback owner invalidates the real transport and prevents a
second Steam client from being initialized while a transport lease remains alive.
Retiring transports deliberately share that one callback/platform owner. They do
not run callbacks or receive/admit native traffic themselves.

## Authenticated connection flow

The listen authority opens `CreateListenSocketP2P` on virtual port 0 by default.
The client immediately connects to the lobby authority's exact Steam networking
identity with `ConnectP2P` after lobby entry. The authority promptly accepts a
current lobby member into a quarantined `ControlReady` connection; this is not
gameplay admission and grants no seat.

Both sides then exchange one-use Steam tickets over the same connection. Before
promotion, the authority must pass the matching `AuthenticatedSteamPeer`
previously consumed from `SteamPlatform`. This direct exchange secures the
physical star link; AFCP v2 subsequently routes recipient-bound tickets between
clients through the authority so every participant authenticates all `N-1`
remote accounts without adding client-to-client sockets. The transport checks all
of the following:

- exact lobby ID;
- exact remote Steam ID;
- callback-authenticated identity equals that Steam ID;
- the Steam license owner is retained as information only and may differ for a
  Steam Families borrower;
- authority/client role and remote identity are independently consistent with
  the immutable session authority.

Pending requests expire after a configurable, bounded timeout (two seconds by
default). Immediately before every listen pump, the coordinator publishes the
exact bounded Steam identities permitted to consume native incoming state.
Before manifest commit this set comes from the coherent Steam roster; after
commit it comes from immutable retained peer bindings so temporary roster-cache
churn cannot block same-identity reclaim. Quality-rejected identities are removed.

The real listener checks this set before allocating a connection record.
Outsiders, malformed identities, duplicates, and excess peers are closed and
counted without producing attacker-amplifiable public events. Callback work is
limited per pump and excess callbacks remain queued for a later pump; ordinary
callback bursts no longer close the listener or healthy connections. A hard
identityless backend/inbox corruption still faults closed. Missing or mismatched
authenticated admissions are rejected. The security phase is monotonic:
`Connecting -> ControlReady -> Authenticating -> Secure -> ManifestAgreement ->
GameplayReceiveArmed -> GameplayReady`. `GameplayReceiveArmed` buffers AFCN for
the exact manifest transaction but exposes no endpoint and permits no gameplay
send. An AFC endpoint is exposed only after explicit `GameplayReady` promotion
following the final activation receipt barrier.

An overlapping connection for the same remote identity is rejected at both the
transport and backend layers by default. `mark_connection_replacement_eligible`
grants one exact connected generation a one-shot exception; the new admission
must be `Reconnect`, and successful link creation consumes the grant. Old and new
connection IDs remain independent, so delayed ACK drain, callback, close, or
object destruction for the old generation cannot mutate the replacement.

Real transport IDs are allocated by the shared Steam-client ownership guard for
the complete client lifetime, not by an individual transport object. Each native
connection receives that ID as Steam connection `user_data`; callbacks resolve
the exact tag and never fall back from a stale tagged callback to a newer link
with the same Steam user. A fresh between-match transport therefore cannot alias
a callback retained for an old retirement.

## AFCP v2 delivery and setup barriers

Lobby schema 4 and AFCP v2 keep all pre-game control on the quarantined star
connections. `RosterPrepare` / `RosterAccepted` freeze a full-roster auth epoch.
`RoutedAuthTicket` retains separate identities for the outer physical hop and the
logical ticket issuer/recipient, plus a stable non-zero logical ticket ID;
`RoutedAuthAccepted` follows the reverse star route. The authority may validate
the outer hop and routing membership, but it treats the recipient-bound ticket
bytes as opaque; only the logical recipient starts the Steam auth session.
`RosterAuthComplete` closes the epoch only after
every participant has validated every other account.

AFCP uses Steam reliable/no-Nagle delivery plus its own bounded application
sequence and cumulative ACK. The sender retains each encoded frame until the
remote application accepts it. Decoding alone does not ACK it: the runtime must
consume the opaque ingress token, at which point the transport first validates
the ACK generation and transmitted upper bound, applies any piggyback ACK, and
queues the inbound sequence ACK. Semantic rejection applies neither ACK and
rejects later ingress for that connection generation. `LinkHello` is always sent
before a standalone ACK. Same-generation duplicate frames and duplicate ACKs are
idempotent.

Manifest control is keyed by a non-zero transaction ID and an immutable set of
`(SteamUserId, SteamConnectionId)` participants. The authority waits for every
`ManifestAccepted`, sends `ManifestCommit`, and waits for every
`ManifestCommitAccepted`. It then arms every exact connection for hidden AFCN
receive buffering before sending `GameplayActivate`. Clients arm before replying
`GameplayActivated` and stay quarantined. After all replies, the authority sends
the same direction-aware `GameplayActivated` as a reliable final release, promotes
its endpoints, and clients promote only upon receiving that release.
Frames for a known retired transaction are still semantically ACKed and ignored:
deleting already sequenced reliable frames would create gaps. `SetupCancel` or a
recoverable abort rolls `ManifestAgreement` and `GameplayReceiveArmed` back to
`Secure` only before the final-release barrier; a handed-off `GameplayReady`
endpoint is never rolled back in place.

## Datagram behavior

Every endpoint implements `NonBlockingDatagramEndpoint`. Both application-facing
directions use bounded synchronous queues, and every per-pump send/receive budget
is bounded. AFC's 1,200-byte datagram ceiling is applied before copying an inbound
Steam message. An oversized Steam message or a full inbound endpoint queue closes
that peer rather than dropping canonical protocol data silently.

Promoted AFC gameplay datagrams use `UNRELIABLE_NO_DELAY`. Reliability, ordering,
retry, sequencing, and acknowledgement remain owned by `NetworkRuntime`; using
Steam reliable mode underneath gameplay would add head-of-line blocking and
duplicate retransmission policy. Quarantined AFCP control frames use
`RELIABLE_NO_NAGLE` plus bounded application sequence/ack retention. A non-AFCP
datagram received before `GameplayReceiveArmed` closes the connection as malformed
traffic. Once armed, valid AFCN is buffered behind the hidden endpoint; local AFCN
sends remain disabled until `GameplayReady`.

Steam send-buffer pressure retains the already-queued datagram for a later pump;
continued pressure fills the bounded endpoint queue and makes subsequent sends
return `SendOutcome::Full` with their original datagram.

Connection close, remote failure, timeout, listener shutdown, and transport fault
release active/pending handles and mark the gameplay endpoint disconnected.
Dropping only the application-facing endpoint is a special bounded case: the
adapter stops backend receive immediately but keeps submitting already-queued
outbound datagrams through the ordinary per-pump budget. It closes after 50 ms
with no pending datagram/queue depth, at a hard 250 ms deadline under persistent
`WouldBlock`, or immediately if Steam reports disconnect/failure.

Whole-match teardown uses the explicit `begin_retirement` /
`pump_retirement` lifecycle instead of ordinary Rust `Drop`. Retirement closes
listener admission, clears public events, and disables all backend/endpoint
receive immediately, but preserves each connected endpoint's already-accepted
outbound queue. Every retirement pump services the bounded send budget before
testing either the per-endpoint deadline or the whole-transport deadline,
including on the exact deadline frame. Each link still uses the 50 ms quiet /
250 ms hard policy; the complete transport has an absolute 300 ms cap.
`SteamTransportRetirementStatus` is one of `Draining`, `Complete`, `TimedOut`, or
`Faulted(error)`, and every terminal value is sticky. Ordinary object `Drop`
remains an emergency close and does not promise a drain.

Together, endpoint and transport retirement let an AFC Disconnect ACK survive
both the worker/endpoint teardown race and the following application-state
transition without turning Steam into a second reliability layer. The pinned
safe wrapper consumes an accepted `ConnectionRequest` until its next connected
callback; during that short transition the adapter retains explicit `Accepting`
state and continues draining callbacks.

## Relay and quality status

The real backend calls both `InitRelayNetworkAccess` and `InitAuthentication`
during construction and polls both readiness states without logging Steam's
unbounded diagnostic string. It exposes bounded enums for overall availability,
certificate/authentication readiness, network configuration, any-relay
reachability, and ping-measurement progress. Online preparation is capped at 15
seconds, and entering Online can retry terminal initialization failures rather
than permanently poisoning the process.

An early transient control-socket failure receives one replacement generation
after an exact 500 ms backoff only when at least five seconds remain in the
original 15-second connection/authentication setup window, which continues after
`ControlReady` until the direct link reaches `Secure`. Both sides retire the old
generation, cancel its ticket/auth state, and issue fresh one-use tickets. The
retry is never used for malformed protocol, identity, ownership, or ticket
failures. A later or second transient failure becomes the lobby's actionable
Retry path. Because clients are the only connection originators, only the client
acts on an exhausted physical-link Retry; the authority passively waits for and
authenticates the replacement. The action retains Steam lobby membership and the
invitation rather than returning to the online menu.

An in-lobby Steam backend disconnect has a distinct 10-second grace. Existing
lobby/auth/socket capabilities and promoted gameplay endpoints remain installed,
and the transport continues pumping, but the authority accepts no new incoming
users and no new setup capability advances. AFC defers bounded auth/setup callback
results and extends setup/activation deadlines by the measured pause. Recovery
must revalidate local membership, lobby metadata, and owner before deferred work
resumes; expiry or failed revalidation leaves the lobby.

For connected peers, `connection_quality` reports sanitized integer metrics:
ping, local/remote delivery rate, packet and byte rates, estimated send rate,
pending reliable/unreliable bytes, unacknowledged reliable bytes, and estimated
queue delay. Invalid, negative, NaN, and infinite native values are not propagated.

`close_connection_for_quality_policy` closes one exact attributable connection
with the bounded `QualityPolicyRejected` reason. It is not encoded as a normal
user-requested close: the real Steam adapter maps it to AFC exceptional
application end code 2010, allowing the coordinator to suppress reconnect policy
for a locally rejected owner link while leaving unrelated listen-owner links open.

## Deterministic fake

`FakeSteamTransportNetwork` creates multiple backend generations for the same
Steam identity without a Steam client. Listener registration, callback inbox,
allowlist, relay state, link side, and injected failure are keyed by the exact
backend generation. Dropping an older generation therefore cannot unregister the
new listener or close the new link. It models listener discovery, pending
requests, explicit acceptance,
connection transitions, bounded wire queues, disconnects, relay status, quality,
explicit quality-policy closes, an exact incoming allowlist, bounded callback
backlog, and injected hard callback corruption. Focused tests prove that outsider
pressure cannot consume pending capacity or prevent a later allowed peer from
connecting, and use its endpoints with the real `NetworkRuntime` packet envelope
and handshake path. Endpoint-drop tests additionally prove queued datagram delivery,
50 ms quiet completion, exact 250 ms hard timeout under permanent backpressure,
immediate backend-failure close, and zero backend receive while draining.
Transport-retirement tests prove sticky complete/timeout/fault outcomes, bounded
send-before-deadline ordering, no receive or event resurrection, and the exact
tracked typed-Disconnect ACK race through a real `NetworkRuntime`. Transport
metrics count `retirements_started`, `retirements_completed`,
`retirement_timeouts`, and `retirement_faults` exactly once per lifecycle.

## Hosted dedicated boundary

This adapter intentionally does not call
`CreateHostedDedicatedServerListenSocket`. Hosted dedicated SDR requires the Steam
GameServer interface plus coordinator-issued signed relay tickets, neither of which
is supplied by the client-owned platform boundary. The capability remains
`UnavailableInPinnedBinding`, and `open_hosted_dedicated_listener` fails explicitly.

`afc-dedicated` does not change this boundary. That executable is an untrusted,
all-bot render-free deployment/test smoke harness with no Steam GameServer
identity, hosted listener, relay ticket, player admission, or result service.
Its successful run is evidence for the shared headless authority loop only, not
for hosted Steam dedicated connectivity or trusted/ranked operation.

## Integrated invariants and external release gate

The native runtime/coordinator enforces these integrated invariants:

1. construct a transport only while the platform is in the exact compatible lobby;
2. close lobby joinability before accepting countdown/start transitions;
3. require role-aware secure-link counts (authority `N-1`, client one) and `N-1`
   verified remote accounts per participant before starting a manifest transaction;
4. send auth tickets only after `AuthTicketReady` and consume platform admission
   before a quarantined connection can become `Secure`;
5. bind manifest acceptance, commit acceptance, receive arming, and activation to
   one immutable participant/connection-generation set;
6. move `AdmittedSteamEndpoint.endpoint` into the matching remote client or listen
   authority and retain its authenticated peer/seat binding; and
7. move the old transport into coordinator-owned retirement during teardown,
   continue bounded outbound-only pumping, and release its match-scoped Steam
   tickets/authentication only after retirement reaches a terminal outcome.

Release still requires invite, launch join, timeout, unplug/reconnect, host loss,
queue pressure, relay status and clean shutdown validation with licensed Steam
accounts on separate machines, including a four-account star/routed-auth run.
