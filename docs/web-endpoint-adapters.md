# Browser multiplayer endpoint adapters

The AFC protocol sees one interface on every platform:
`NonBlockingDatagramEndpoint`. Web transport code carries each opaque AFC
datagram as exactly one transport message and does not decode canonical
messages, replace AFC sequence numbers, or change the 1,200-byte ceiling.

## Browser

`connect_browser_datagram_endpoint` supports three policies:

- `WebTransportPreferred` tries an HTTPS WebTransport session first and falls
  back to a WSS WebSocket connection if the browser or network cannot establish
  HTTP/3.
- `WebTransportOnly` is useful for capability and latency testing.
- `WebSocketOnly` provides the broad compatibility path.

WebTransport uses the browser datagram readable stream and a single serialized
writable-stream writer. The adapter accepts an AFC send only after placing it in
its bounded outbound queue. One asynchronous writer drains that queue in order;
write failure closes the transport and becomes a terminal endpoint error. It
uses `datagrams.createWritable()` where available and the older `writable`
property as a compatibility fallback. A negotiated datagram maximum below
1,200 bytes is rejected before protocol startup.

WebSocket uses binary `ArrayBuffer` messages and the fixed subprotocol
`afc.datagram.v1`. `bufferedAmount` is capped independently of AFC's packet
queues. Text messages, oversized frames, callback queue overflow, and browser
transport errors fail the endpoint closed. WebSocket's reliable ordering is an
extra transport property; the unchanged AFC runtime still owns application
acknowledgements, retry, sequencing, and channel semantics.

Every connection requires HTTPS/WSS in production. Itch.io embeds the game from
a different origin, so the API origin must explicitly allow the configured
Itch origin; wildcard credentials are not acceptable.

## Hosted server

`ServerDatagramBridge::pair` returns:

- a synchronous `ServerDatagramEndpoint` owned exclusively by
  `AuthorityPeerHub`; and
- an asynchronous bridge owned exclusively by the connection task.

The bridge uses bounded Tokio channels in both directions. Axum WebSocket and
`wtransport` WebTransport runners translate complete binary messages/datagrams
to the bridge. If inbound capacity is exhausted, the connection is terminated
instead of dropping an AFC control/result packet. Disconnect state is shared
atomically, and already-queued inbound packets drain before the endpoint reports
disconnection.

The HTTP WebSocket listener and HTTP/3 WebTransport listener may use distinct
TCP and UDP ports. Production routing must expose both, negotiate
`afc.datagram.v1` for WebSockets, and use a publicly trusted TLS certificate for
WebTransport. Authentication and room admission happen before either bridge is
attached to an authority peer.
