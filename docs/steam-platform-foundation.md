# Steam Platform Foundation

- Status: **implemented and native-application wired; two-account release validation pending**
- Binding: `steamworks = 0.12.2` (exact optional pin)
- Feature: `steam-net`
- Source: `src/steam_platform.rs`
- Decision date: 2026-07-23

This boundary owns Steam client initialization, the one callback pump, lobby policy,
invitations, launch joins, rich presence, and authentication sessions. It does not
own simulation, seat assignment, prediction, rollback, or gameplay packets. UDP and
in-process transports remain the deterministic test paths.

## Accepted scope

The first online milestone supports private and friends-only lobbies. Public lobby
metadata is representable, but `SteamClientConfig` disables public lobbies unless a
caller explicitly enables the separate product gate. App ID is mandatory
configuration: there is no shipping default. A shipping release uses
`--release --no-default-features --features shipping`, with both
`AFC_BUILD_ID=<immutable-label>` and `AFC_STEAM_APP_ID=<real>` set at compile
time. `build.rs` validates and embeds them, and binds them into the multiplayer
build identity. A redundant runtime value must equal the embedded value, while a
runtime-only value is development-only. Spacewar App ID 480 is rejected in
production and works in development only with the explicit development opt-in.

The raw release executable is not a depot. `scripts/release.py audit-source`
requires a clean commit; `stage`, `verify`, and `archive` query the executable's
deterministic `--release-identity`, assemble only the player payload, and seal
the manifest and SHA-256 contract. See
[Native release packaging](release-packaging.md).

The player executable performs `SteamAPI_RestartAppIfNecessary` before constructing
Bevy or initializing `steamworks::Client`. If Steam requests a relaunch, `main`
returns immediately; that process must never continue into the Online unavailable
screen. Physical candidate acceptance must include both an env-free depot launch
and an outside-Steam launch that is handed back to Steam. Development
`steam_appid.txt` files are never part of a depot.

`SteamPlatform<B>` is deliberately not cloneable. It owns the backend, active auth
tickets/sessions, and bounded public event queue. `RealSteamBackend` keeps the
`steamworks::Client` private, and only `SteamPlatform::pump` invokes its callback
pump. Default CI uses `FakeSteamBackend`; no Steam client or account is required.

Valve specifies that joining completes asynchronously, metadata is available at
`LobbyEnter_t`, roster changes arrive through `LobbyChatUpdate_t`, and only the
lobby owner may change lobby metadata. AFC validates the exact known schema after
joining and before creating any gameplay transport admission. Friends-only lobbies
also require a current Steam-friend relationship; private lobbies require an
invite, rich-presence join, or `+connect_lobby` launch intent. See [Steam
matchmaking and lobbies](https://partner.steamgames.com/doc/features/multiplayer/matchmaking)
and the [ISteamMatchmaking API](https://partner.steamgames.com/doc/api/ISteamMatchmaking).

## Bounded lobby schema

The platform reads individual known keys; it never enumerates or accepts arbitrary
lobby data. Each value is at most 128 bytes and contains no NUL. Region is 1–16
lowercase ASCII letters, digits, or hyphens. A lobby has at most four Steam peers
and four total local seats.

| Scope | Key | Value |
| --- | --- | --- |
| Lobby | `afc_schema` | Decimal schema version (`2`) |
| Lobby | `afc_build` | 16-byte AFC build ID as 32 lowercase hex digits |
| Lobby | `afc_protocol` | Non-zero decimal protocol version |
| Lobby | `afc_sim` | Non-zero decimal simulation version |
| Lobby | `afc_replay` | Non-zero decimal replay version |
| Lobby | `afc_content` | 32-byte gameplay content hash as 64 lowercase hex digits |
| Lobby | `afc_authority` | `listen` or `dedicated` |
| Lobby | `afc_visibility` | `private`, `friends`, or `public` |
| Lobby | `afc_region` | Validated region code |
| Lobby | `afc_rules` | Decimal stable rules definition ID |
| Lobby | `afc_arena` | Decimal stable arena definition ID |
| Lobby | `afc_seats` | Total seat capacity, 1–4 |
| Lobby | `afc_admission` | Owner's desired new-peer admission policy (`0` or `1`) |
| Lobby | `afc_open` | Effective joinability after policy, peer, coherence, and seat-capacity checks |
| Member | `afc_ready` | Transaction marker: `s:<revision>` or `c:<revision>:<ready>` |
| Member | `afc_local_seats` | Seats owned by this Steam peer, 1–4 |
| Member | `afc_loadout` | Versioned canonical seat/loadout declaration, at most 64 lowercase hex characters |

Every immutable lobby compatibility field must exactly equal
`current_compatibility()`. A missing, partial, malformed, duplicate, over-capacity,
or incompatible lobby contract fails the session closed. The immutable contract is
retained on entry and re-read exactly on every lobby-subject metadata callback and
immediately before authentication or manifest commit. Steam's callback subject is
preserved: a member-subject update cannot be mistaken for permission to accept a
concurrently changed lobby contract.

Member declarations are transactional. A writer publishes `s:<revision>`, then
the seat count and canonical loadout, then `c:<revision>:<ready>`. Observers accept
only a committed marker whose revision equals the loadout revision and whose seat
count equals both the decoded loadout and `afc_local_seats`. Staged, missing, or
partially visible values project that member as pending; they never become globally
fatal and never count as ready. A ready toggle changes only the committed marker at
the already accepted revision.

Each member has a last-accepted continuity cache. Lower revisions and same-revision
content conflicts are rejected for that member, retain only the accepted history,
and require a strictly higher coherent revision to recover. Malformed and capacity
rejections are deduplicated in the bounded event stream. Capacity is recalculated
from one raw snapshot of every current member on every member or membership
callback. Arbitration is deterministic—lobby owner first, then ascending Steam
user ID—so callback arrival order cannot decide which declarations fit.

`afc_admission` and `afc_open` deliberately have different meanings. The former is
the owner's desired admission policy. The latter is true only when that policy is
enabled, fewer than four Steam peers are present, every current member declaration
is coherent, and accepted seats remain below `afc_seats`. Closing updates native
Steam joinability before publishing `afc_open=0`; opening publishes
`afc_open=1` before enabling native joinability. Create and join never publish
legacy or partial member declarations.

## Invite, launch, and presence flow

The real backend handles `GameLobbyJoinRequested_t`,
`GameRichPresenceJoinRequested_t`, and `NewUrlLaunchParameters_t`. It also reads
`ISteamApps::GetLaunchCommandLine` at startup. The parser accepts exactly one
`+connect_lobby <non-zero decimal u64>` token inside a command of at most 96 bytes;
ambiguous, malformed, control-character, and oversized inputs are rejected.
Valve documents this launch form and recommends `GetLaunchCommandLine` for lobby
invites: [ISteamApps](https://partner.steamgames.com/doc/api/ISteamApps) and
[ISteamFriends](https://partner.steamgames.com/doc/api/ISteamFriends).

While a lobby is admitting peers, rich presence publishes `status`,
`steam_player_group`, `steam_player_group_size`, and the bounded `connect` string.
The `connect` key is cleared before countdown, teardown, or Steam disconnect.
After a valid Steam owner transfer, presence is cleared and rebuilt from the
retained lobby admission state; a committed match was already closed to new
members. A partial API failure therefore cannot leave a stale join link. Valve
documents the special presence keys, their limits, and `ClearRichPresence` in
[ISteamFriends](https://partner.steamgames.com/doc/api/ISteamFriends).

The safe pinned Rust API exposes Steam's lobby invite overlay, so this foundation
uses that supported path. It does not pretend to offer a direct per-friend lobby
invite call that the pinned safe wrapper does not expose.

Every user request rechecks `ISteamUtils::IsOverlayEnabled` after validating the
current Lobby phase and effective joinability. Steam may report false for the
first few seconds while the overlay hooks the process, so readiness is never
cached and never hides an otherwise eligible Invite action. A disabled invite
returns the typed `Unavailable` status without mutating lobby/session state;
the application presents a sanitized dismissible notice for four seconds. The
void invite API is reported as `Submitted`, not as proof that the dialog became
visible.

`GameOverlayActivated_t` is retained as one coalesced latest boolean rather than
placed in the semantic callback FIFO. It is not an invite-completion signal.
While active during online combat, local gameplay input is submitted as neutral,
but callback, transport, authority/client, fixed simulation, and rendering pumps
continue normally.

## Authentication and expected-lobby admission

Lobby membership is never, by itself, authority admission. The authority performs
this sequence for every transport peer:

1. Confirm the exact active lobby and a bounded, validated member seat declaration.
2. Record an initial or reconnect admission intent with a 15-second deadline.
3. Pass the peer's bounded ticket to `BeginAuthSession` using that claimed Steam ID.
4. Wait for a successful `ValidateAuthTicketResponse_t`; synchronous success from
   `BeginAuthSession` is not identity confirmation. The callback's `m_SteamID` is
   the ticket provider and must match that claimed peer.
5. Require `UserHasLicenseForApp(configured_app_id) == HasLicense`.
6. Consume the admission once and map the non-zero Steam ID to
   `AuthenticatedUserId`; only then may session code assign a `PeerId`/seats.

`m_OwnerSteamID` is retained as `license_owner_user`, not interpreted as the
lobby authority. It names the account that owns the app license and may differ
from the ticket provider when the game is borrowed through Steam Families. The
first-release policy permits that Steam-supported borrowing when ticket
validation succeeds and `UserHasLicenseForApp` reports `HasLicense`; lobby and
gameplay authority continue to come independently from the immutable lobby/session
contract. Reliable duplicate delivery of the same lobby/user/purpose ticket is
idempotent before and after one-time admission consumption and never creates a
second auth session.

Valve explicitly says identity is not confirmed until the validation callback,
requires `EndAuthSession` at teardown, and requires ticket issuers to call
`CancelAuthTicket`: [ISteamUser](https://partner.steamgames.com/doc/api/ISteamUser)
and [user authentication and ownership](https://partner.steamgames.com/doc/features/auth).
The service keeps approved auth sessions alive after one-time admission consumption
so later revocation callbacks still reject the peer. Leaving, member removal,
timeout, fault, and drop cancel/end every retained handle/session.

The pinned wrapper allocates a 1024-byte ticket buffer. AFC rejects an empty or
larger ticket instead of truncating or silently accepting it. Valve notes that
unusually DLC-heavy applications can require a larger buffer; changing that limit
requires a reviewed binding update and a boundary test.

Ticket bytes are secret-bearing owned values. Neither backend-issued nor
platform-issued tickets implement production cloning or byte-revealing `Debug`;
diagnostics expose only the handle, remote identity where applicable, byte count,
and `<redacted>`. Ownership moves from backend to platform to the versioned
pre-game envelope. Every owned ticket/envelope buffer is explicitly overwritten
through an optimization-resistant zeroization boundary on drop, and error paths
zeroize temporary moved buffers before returning. Callback correlation and later
`CancelAuthTicket` use the non-secret handle rather than retaining an extra byte
copy.

## Failure and authority-loss behavior

The real backend does not place unrelated callback types into one shared FIFO.
It uses a fixed semantic mailbox:

- create/join completions are keyed by operation ID and drained before a terminal
  disconnect/integrity callback, so successful native membership is still left;
- cancel, timeout, disconnect, and teardown retire their operation slot
  immediately. A monotonic bounded tombstone makes a delayed completion benign,
  and a delayed successful lobby is left unless it is already the current scope;
- auth-ticket responses are keyed by handle, and auth validation is keyed by the
  ticket provider, with rejection/revocation dominating success;
- a same-user auth session cannot start while an older generation is waiting for
  its first callback, preventing delayed callback misattribution;
- active-lobby departures remain sticky, while membership and lobby/member-data
  chatter coalesces into bounded authoritative rereads;
- invite/rich-presence intents are latest-wins, and malformed external connect
  commands are ignored without faulting an active match; and
- Steam disconnect and identityless integrity failure remain sticky terminals.

Attributable callback volume therefore cannot become a process-global callback
overflow. A poisoned mailbox, malformed callback identity, conflicting
capability completion, malformed active-lobby data, unexpected async result, or
public event overflow still faults the service, clears rich presence, cancels
authentication, and leaves the lobby. The deterministic fake retains an explicit
hard-overflow injection so fail-closed infrastructure behavior remains testable.
Routine create/join refusal returns to idle with an explicit bounded event.

All deadlines use caller-supplied monotonic milliseconds. A time regression faults
the service instead of extending a join or authentication intent.

If the listen authority disappears, AFC refreshes the bounded roster, asks Steam
for the replacement lobby owner, and revalidates the immutable lobby metadata.
A valid surviving replacement is installed atomically with an `AuthorityLost`
event containing both old and new owner IDs; the platform remains `InLobby`.
Missing/non-member replacements or changed metadata fault closed and leave.

This owner transfer never migrates the active simulation authority. The
coordinator ends an active match as no-contest and creates no replacement
transport until the player returns to the lobby. Only then can the Steam-selected
owner start a fresh between-match listen authority.

## SDR capability boundary

Steam peer-to-peer Networking Sockets can automatically use SDR. AFC now owns an
auth-gated adapter in `src/steam_transport.rs`; see
[Steam gameplay transport](steam-gameplay-transport.md). The audited stock
Lightyear adapter still auto-accepts sessions before this admission service and
remains spike-only.

Hosted dedicated SDR requires the Steam GameServer hosted-address/listen-socket
flow and, for ticket-based connections, a game coordinator that signs relay auth
tickets. Valve describes `GetHostedDedicatedServerAddress`,
`CreateHostedDedicatedServerListenSocket`, and the coordinator ticket flow in
[Steam Datagram Relay](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)
and [ISteamNetworkingSockets](https://partner.steamgames.com/doc/api/ISteamNetworkingSockets).
This AFC `steamworks` 0.12.2 client boundary does not expose or implement that
production service. `dedicated_hosted_sdr_support()` therefore returns
`UnavailableInPinnedBinding`; trusted/ranked/dedicated Steam play stays disabled.

## Native application composition and release validation

`NativeOnlineRuntime` now constructs the one `SteamPlatform<RealSteamBackend>`
from explicit App ID configuration and retains sole callback ownership.
`drive_native_online_application` pumps it once per application frame.
`OnlineLobbyCoordinator` drains the typed platform events, while the bounded
NetworkingMessages bootstrap transmits tickets and the canonical manifest only
after the matching platform gate. `NativeOnlineApplication` moves admitted
endpoints into `ListenOnlineMatch` or `RemoteOnlineClient` and forwards auth
revocation/platform-ban events to the authority. The custom transport is created
with `SteamTransport::from_steam_platform`; the stock Lightyear automatic Steam
acceptance path is not used.

Automated fake-platform and fake-transport tests cover the guarded composition,
but they cannot validate Valve's live services. The protected native candidate
workflow builds Windows, universal macOS, and Linux inside the release-policy
pinned Steam Linux Runtime 4 SDK, then re-verifies the archives and exact
cross-platform release identity. Its generated SteamPipe VDFs are preview-only;
the repository does not provide signing credentials or product App/depot IDs,
and the workflow cannot upload content to Steam or promote a branch. A protected
environment must supply the approved IDs before the workflow can build.

Run the two-account/machine invite, launch, ready, match, rematch, reconnect,
host-loss, return-to-lobby, physical controller, and clean shutdown gates on the
sealed candidate before release. Configure Steam Linux Runtime 4 (runtime App ID
4183110) separately in Steamworks Partner; it is not AFC's compiled product App
ID. Record external signing/notarization, depot build IDs, upload preview, and
branch promotion separately in
[Steam release acceptance](steam-release-acceptance.md). A later dedicated
implementation needs Valve partner coordination/game-coordinator credentials
and separate hosted-SDR tests.
