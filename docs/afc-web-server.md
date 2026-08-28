# Hosted browser server

`afc-web-server` is the player-facing hosted authority. It is independent of
the deployment/test-only `afc-dedicated` bot smoke binary. Each sealed room
constructs its own headless simulation and `AuthorityPeerHub` on a fixed 60 Hz
worker; HTTP and transport tasks never execute canonical gameplay.

## Public API

All responses use `Cache-Control: no-store`. Browser origins are matched
exactly against `AFC_WEB_ALLOWED_ORIGINS`; wildcards are rejected. Room calls
use `Authorization: Bearer <guest-session>` and never accept credentials or
join tickets in URLs.

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/healthz` | Process liveness |
| `GET` | `/readyz` | Admission readiness |
| `GET` | `/metrics` | Non-secret Prometheus counters/gauges |
| `GET` | `/v2/config` | Versioned API, release, and transport discovery |
| `POST` | `/v2/guests` | Validate a nickname and issue a 24-hour guest bearer |
| `GET` | `/v2/lobby` | Current public-room directory, chat, presence, and active room |
| `GET` | `/v2/lobby/ws` | Authenticated JSON lobby/chat stream with `afc.lobby.v2` |
| `POST` | `/v2/rooms` | Create a public or code-only room for two to four players |
| `POST` | `/v2/rooms/join` | Join by 12-symbol room code |
| `GET` | `/v2/rooms/{code}` | Read the caller-specific room and authority state |
| `PATCH` | `/v2/rooms/{code}/settings` | Host selects arena and rules at an expected revision |
| `PATCH` | `/v2/rooms/{code}/members/self/character` | Guest selects a character at an expected revision |
| `PATCH` | `/v2/rooms/{code}/members/self/ready` | Guest changes ready state at an expected revision |
| `POST` | `/v2/rooms/{code}/members/kick` | Host removes and room-bans a member |
| `POST` | `/v2/rooms/{code}/start` | Host seals the ready immutable roster and starts a worker |
| `POST` | `/v2/rooms/{code}/results/ack` | Member acknowledges the authority-confirmed result |
| `POST` | `/v2/rooms/{code}/leave` | Leave an open room; host ownership migrates if needed |
| `POST` | `/v2/rooms/{code}/tickets` | Issue a one-time, short-lived initial/reconnect ticket |
| `GET` | `/v2/connect/ws` | Binary WebSocket gameplay with `afc.datagram.v1` |
| HTTP/3 | `/v2/connect/wt` | WebTransport gameplay session |

WebSocket admission is the first binary message. WebTransport admission uses
one client-initiated reliable bidirectional stream. Only after the server
verifies and consumes the signed room/match/peer/mode ticket does it attach the
bounded endpoint, return `AFCO\x01`, and allow AFC datagrams.

`TicketResponse.countdown_start_tick` is present for reconnect tickets and is
the authenticated authority-selected boundary needed to rebuild the lost
browser prediction process. `RoomWorkerResponse.countdown_start_tick` exposes
the same non-secret progress value for lobby/UI state. A reconnect client must
not infer this boundary from its fresh local clock or from the current worker
tick.

The itch bundle discovers this API from the `afc_server` query override,
`window.AFC_WEB_API_URL`, or the `afc-web-api-url` meta element in
`web/index.html`, in that priority order, then falls back to the page origin.
Guest bearers are retained only in per-tab `sessionStorage` and WASM state so a
refresh can reclaim the same room; they are never projected through the DOM
bridge. Closing the tab clears the browser copy. HTTPS pages reject an HTTP API
origin, the API performs exact Origin matching, and the browser refuses
release/subprotocol/API mismatches before it requests a one-time ticket.

The control socket authenticates with the bearer as its first bounded JSON
message. It carries lobby snapshots, revision invalidations, global/room chat,
reports, resync requests, and ping/pong only; it never carries canonical
gameplay. Room mutations use optimistic revisions, readiness is cleared after a
roster/loadout/settings change, and start freezes the manifest before any
gameplay ticket is issued. A confirmed result remains visible for a bounded
period, then every participant returns to the same reusable room with readiness
cleared. A disconnected host has a 30-second presence grace before ownership
migrates to the longest-present remaining member.

## Production configuration

Production mode is the default and requires both public transports. Terminate
public HTTPS/WSS at a reverse proxy or load balancer and forward HTTP/1.1 with
upgrade support to the TCP listener. Forward the configured UDP port directly
to `afc-web-server`; the WebTransport listener owns TLS/HTTP3 and therefore
needs the public certificate chain and private key. The public WebSocket URL
must use `wss://`; the WebTransport URL must use `https://`.
The image runs as UID/GID `10001`; certificate and key mounts must be readable
by that identity (prefer a narrowly scoped group or ACL instead of making the
private key world-readable).

Production mode also rejects a debug binary or the mutable `development`
release label. Build the itch client and authority from the same committed
source, in the release profile, with exactly the same immutable label:

```sh
AFC_BUILD_ID=<IMMUTABLE_RELEASE_LABEL> ./scripts/build_web.sh
AFC_BUILD_ID=<IMMUTABLE_RELEASE_LABEL> \
  docker compose -f deploy/compose.web-server.yml build
```

Compatibility digest v3 intentionally normalizes only the `web` versus
`web-server` delivery role, so the resulting artifacts match while every other
feature and build input remains strict. A mismatched client fails closed before
requesting a one-time join ticket.

When HTTP/WSS is behind a proxy, set `AFC_WEB_TRUSTED_PROXY_IPS` to the exact
comma-separated socket IPs of those proxies. Only those peers may supply an
`X-Forwarded-For` chain; the server walks the chain from the trusted edge and
uses the first untrusted hop for per-client rate limiting. Leave the variable
empty for direct connections. Configure the proxy to append or replace this
header and never expose the internal TCP listener publicly when trust is on.

The signing secret is exactly 32 random bytes encoded as unpadded base64url.
Prefer `AFC_WEB_SIGNING_KEY_FILE` (a Docker/Kubernetes secret mount) over an
environment value. Rotate by deploying a new current key ID/key while keeping
the former pair in `AFC_WEB_PREVIOUS_SIGNING_KEY_ID` plus either
`AFC_WEB_PREVIOUS_SIGNING_KEY` or its `_FILE` form. Remove the previous key
after the 24-hour guest-session lifetime has elapsed.

The complete variable list is printed by:

```sh
cargo run --locked --no-default-features --features web-server \
  --bin afc-web-server -- --help
```

Validate a deployment without opening sockets:

```sh
cargo run --locked --no-default-features --features web-server \
  --bin afc-web-server -- --check-config
```

`deploy/compose.web-server.yml` requires `AFC_BUILD_ID` and provides a non-root,
read-only container with
dropped capabilities, health checks, a mounted signing secret, TCP/UDP port
separation, and a 20-second graceful stop window. Its HTTP port binds host
loopback by default; set `AFC_WEB_HTTP_HOST_IP` only when the reverse proxy
cannot reach loopback, and firewall the resulting bind from public traffic.
Rooms are deliberately process-local and ephemeral: deploy one authority
process per routing shard, use sticky routing for the HTTP/WebSocket hostname,
drain readiness before replacement, and never split one room across replicas.
One process admits at most 100 current guest profiles and 25 rooms by default;
each room admits at most four players. These are safety ceilings, not a promise
that one instance meets a target concurrency on unmeasured hardware. Establish
per-instance limits with the four-client worker soak and server-tick/RSS metrics
before routing production traffic.

## Operational acceptance

Before production traffic:

1. Confirm `/readyz` returns 200 through the public HTTPS endpoint.
2. Confirm the configured itch origins receive the exact CORS origin and an
   unlisted origin receives 403.
3. Complete nickname → lobby chat → public discovery → private-code join →
   character/ready/start → battle → result → same-room return → rematch with two
   isolated browsers over WSS.
4. Repeat the complete lifecycle with four isolated browsers and retain browser
   logs, screenshots, metrics, room revisions, result IDs, and server logs.
5. Complete the same admission, gameplay input, result, and return over the
   public WebTransport/UDP endpoint. Confirm metrics show WebTransport admission
   rather than WebSocket fallback.
6. Verify TCP and UDP load-balancer idle timeouts exceed 30 seconds.
7. Send SIGTERM while a room is active and verify readiness changes to 503,
   peers receive authority shutdown, and the process exits within 20 seconds.
8. Alert on admission rejection, rate-limit, active-session, transport-adapter
   error, failed-room, and server-tick distribution signals.

The checked-in Playwright scenario is the reproducible local and CI acceptance
driver. On macOS with OrbStack running, serve the already-built `web_dist/`, run
the matching image on loopback, and execute the two-, three-, and four-client
matrix with real Chrome:

```sh
npm ci
python3 -m http.server 8000 --directory web_dist

AFC_QA_CLIENTS=2 \
AFC_QA_OUTPUT_DIR="$PWD/target/qa/web/playwright-2" \
AFC_WEB_BASE_URL=http://127.0.0.1:8000 \
AFC_WEB_SERVER_ORIGIN=http://127.0.0.1:18080 \
AFC_CHROMIUM_EXECUTABLE="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  npx playwright test --config tests/browser/playwright.config.mjs
```

Repeat with `AFC_QA_CLIENTS=3` and `4`, using a fresh server process between
runs so presence, rooms, tickets, and metrics are isolated. The scenario drives
lobby and room chat, public/private discovery, host settings, every player's
character/readiness, refresh reconnect, controlled movement/attacks/jumps, an
authority-confirmed result, same-room return, and a second complete match. It
fails on browser page/console errors and writes screenshots, traces, network
logs, room/result snapshots, and metrics below `target/qa/web/`. The hosted
authority workflow runs all two-, three-, and four-client cases on every
relevant pull request and retains the same evidence as a CI artifact.
