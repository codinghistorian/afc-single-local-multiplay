# Hosted private-room lifecycle

`WebRoomService` owns a bounded in-memory registry for casual guest rooms. A
room moves monotonically through these states:

1. `Open`: the creator is host; anyone holding the 60-bit private code may join
   until the configured 2–4 player capacity is reached.
2. `Starting`: only the host may seal the roster. Concurrent starts and joins
   fail; the service constructs the headless world on a dedicated worker.
3. `Active`: roster, peer IDs, seat ownership, manifest, content identity, and
   gameplay seed are immutable. Each member may request a short-lived initial
   or reconnect ticket.
4. `Finished`/`Failed`: the worker retains bounded diagnostics/results for the
   configured terminal retention interval, then drains and retires.

Peer IDs are assigned once when a guest joins and never renumbered. Sealing
maps members in join order to fighter seats and builds the manifest through
`build_headless_match_config`; there is no alternate web simulation schema.
Guest rooms use `AuthorityKind::Dedicated` but `trusted_results = false`, so a
server-authoritative casual result cannot accidentally become a durable ranked
or reward claim.

Every active room has exactly one OS thread that constructs and owns its Bevy
headless simulation and `AuthorityPeerHub`. Its rational clock maps tick 60 to
exactly one SI second, never skips canonical ticks, and catches up in bounded
yielding bursts after scheduling delays. Bounded commands transfer admitted
`ServerDatagramEndpoint` values into that owner; async socket tasks never touch
the simulation or hub directly.

Open rooms, terminal rooms, the total registry, replay nonces, per-room members,
worker commands, and transport queues all have explicit limits. A hard room
lifetime prevents abandoned workers from surviving indefinitely. Process
shutdown invokes the dedicated hub drain path, which sends typed terminal
messages to every peer and has no listen-host exemption.

The initial implementation is intentionally ephemeral: a service restart ends
active matches and discards room codes. Horizontal production deployment must
therefore use connection-affine routing for the lifetime of a room, or add a
separate coordinator before scaling beyond one authority process. Canonical
match state must not be migrated by serializing the Bevy world ad hoc.
