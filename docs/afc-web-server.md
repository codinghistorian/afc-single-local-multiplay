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
| `GET` | `/v1/config` | Versioned public transport discovery |
| `POST` | `/v1/guests` | Issue a 24-hour guest bearer session |
| `POST` | `/v1/rooms` | Create a private room |
| `POST` | `/v1/rooms/join` | Join by 12-symbol private code |
| `GET` | `/v1/rooms/{code}` | Read member and authority state |
| `POST` | `/v1/rooms/{code}/start` | Host seals the immutable roster |
| `POST` | `/v1/rooms/{code}/leave` | Leave an open lobby; host closes it |
| `POST` | `/v1/rooms/{code}/tickets` | Issue a one-time, short-lived join ticket |
| `GET` | `/v1/connect/ws` | Binary WebSocket with `afc.datagram.v1` |
| HTTP/3 | `/v1/connect/wt` | WebTransport session |

WebSocket admission is the first binary message. WebTransport admission uses
one client-initiated reliable bidirectional stream. Only after the server
verifies and consumes the signed room/match/peer/mode ticket does it attach the
bounded endpoint, return `AFCO\x01`, and allow AFC datagrams.

The itch bundle discovers this API from the `afc_server` query override,
`window.AFC_WEB_API_URL`, or the `afc-web-api-url` meta element in
`web/index.html`, in that priority order, then falls back to the page origin.
Guest bearers remain only in WASM memory. HTTPS pages reject an HTTP API
origin, the API performs exact Origin matching, and the browser refuses
release/subprotocol/API mismatches before it requests a one-time ticket.

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

## Operational acceptance

Before production traffic:

1. Confirm `/readyz` returns 200 through the public HTTPS endpoint.
2. Confirm the configured itch origins receive the exact CORS origin and an
   unlisted origin receives 403.
3. Complete guest → create/join → start → two ticket admissions over WSS.
4. Complete the same admission over public WebTransport/UDP.
5. Verify TCP and UDP load-balancer idle timeouts exceed 30 seconds.
6. Send SIGTERM while a room is active and verify readiness changes to 503,
   peers receive authority shutdown, and the process exits within 20 seconds.
7. Alert on admission rejection, rate-limit, active-session, transport-adapter
   error, failed-room, and server-tick distribution signals.
