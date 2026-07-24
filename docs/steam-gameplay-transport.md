# Steam Gameplay Transport

- Status: **implemented production listen path; native two-account release validation pending**
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
The client connects to the lobby authority's exact Steam networking identity with
`ConnectP2P`. An inbound `Connecting` callback is retained as
`PendingAdmission`; it is never accepted automatically.

The authority must pass the matching `AuthenticatedSteamPeer` previously consumed
from `SteamPlatform`. The transport checks all of the following again before
calling `AcceptConnection`:

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
authenticated admissions are rejected. An AFC endpoint is exposed only after
Steam reports the connection as `Connected`.

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

## Datagram behavior

Every endpoint implements `NonBlockingDatagramEndpoint`. Both application-facing
directions use bounded synchronous queues, and every per-pump send/receive budget
is bounded. AFC's 1,200-byte datagram ceiling is applied before copying an inbound
Steam message. An oversized Steam message or a full inbound endpoint queue closes
that peer rather than dropping canonical protocol data silently.

The adapter sends `UNRELIABLE_NO_DELAY`. Reliability, ordering, retry, sequencing,
and acknowledgement remain owned by `NetworkRuntime`; using Steam reliable mode
underneath it would add head-of-line blocking and duplicate retransmission policy.
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

The real backend calls `InitRelayNetworkAccess` during construction and polls the
detailed relay status without logging Steam's unbounded diagnostic string. It
exposes bounded enums for overall availability, network configuration, any-relay
reachability, and ping-measurement progress.

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
3. send auth tickets only after `AuthTicketReady` and consume platform admission
   before `admit_incoming` or `connect_p2p`;
4. move `AdmittedSteamEndpoint.endpoint` into the matching remote client or listen
   authority and retain its authenticated peer/seat binding; and
5. move the old transport into coordinator-owned retirement during teardown,
   continue bounded outbound-only pumping, and release its match-scoped Steam
   tickets/authentication only after retirement reaches a terminal outcome.

Release still requires invite, launch join, timeout, unplug/reconnect, host loss,
queue pressure, relay status and clean shutdown validation with two licensed Steam
accounts on separate machines.
