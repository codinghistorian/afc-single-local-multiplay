# Online Roster and Manifest Contract

- Status: implemented and native-application wired; two-account Steam validation pending
- Source: `src/online_roster.rs`
- Decision date: 2026-07-23

The lobby is mutable; a match manifest is not. `OnlineRoster` is the bounded seam
between authenticated Steam lobby membership and the authority's immutable
`MatchManifest`.

Each member record binds all of the following:

- protocol `PeerId`;
- authenticated platform user ID;
- monotonically increasing declaration revision;
- readiness;
- one to four local couch-co-op seat loadouts.

No roster may exceed four members or four total fighter seats. A peer ID and an
authenticated user ID may each appear only once. Reusing a peer ID for another
authenticated identity, replaying a declaration revision, or increasing the total
seat count past four fails closed. Changing any seat/loadout publishes a new
revision and should clear readiness in the application flow.

## Steam member declaration

The loadout declaration has a canonical lowercase-hex representation small enough
for one bounded Steam member-metadata value. The decoded bytes are:

```text
version:u8 (=1)
revision:u16 little-endian (non-zero)
seat_count:u8 (1..=4)
repeated seat_count times:
  team:u8
  character:u16 little-endian
  style:u16 little-endian
  equipment:u16 little-endian
```

The largest declaration is 32 bytes / 64 lowercase hexadecimal characters.
Uppercase, odd length, unknown version, non-canonical trailing bytes, invalid IDs,
zero revision, and over-capacity seat counts are rejected. Steam identity and the
ready bit are deliberately not trusted from this payload: they come from the
authenticated lobby member and separately validated member metadata.

## Canonical fighter assignment

Callback/arrival order never controls fighter identity. For a listen match, the
authenticated authority peer is ordered first so its local couch seats retain
stable player-one UX. Remaining peers are ordered by numeric `PeerId`. Dedicated
matches order every peer by `PeerId`. Each member's local seats retain their
declared order, and the resulting sequence is assigned contiguous `SeatId` and
`FighterId` values from zero.

The manifest can be built only when every retained member is ready. Listen mode
requires the named authority to be a roster member. Dedicated mode forbids a peer
authority. Offline mode is invalid at this boundary. The shared manifest validator
also forbids trusted results on a listen authority.

For the first player-facing Steam release, `FirstReleaseOnlinePolicy` narrows
that general protocol model further: only untrusted listen options and manifests
are admitted. Its public, hosted-dedicated, ranked, and trusted capability flags
are all false. Dedicated manifests remain representable for future server work
and internal headless tests, but cannot cross the native lobby coordinator.

Manifest construction uses the same compatibility digest, canonical hash,
definition mapping, headless bootstrap conversion, timing limits, and trusted-mode
policy as local and dedicated play. Reversing roster insertion order therefore
produces the identical manifest and manifest hash.

## Application flow

1. Steam validates lobby membership, seat-count metadata, authentication ticket,
   AppID ownership, and any platform ban result. In the client-to-owner topology,
   a client authenticates its own connection and the lobby owner; it does not open
   authentication sessions to unrelated third-party lobby members.
2. The platform owner maps each approved identity to one stable `PeerId` and
   upserts the canonical loadout declaration.
3. A loadout change increments revision and clears ready. A staged, missing, or
   otherwise incomplete declaration keeps manifest agreement pending rather than
   accepting a partial roster.
4. The listen owner closes admission only after the complete roster is ready.
5. The authority builds exactly one manifest and sends that same value through the
   AFC startup handshake to every peer.
6. Each client pins mappings it knows cryptographically: its own Steam identity to
   its own manifest peer and the lobby owner to the authority peer. It then matches
   every other current lobby member to an unused manifest peer group by the exact
   ordered couch-seat loadout signature. Identical unknown signatures are
   interchangeable; a known mapping can never be stolen by such a match.
7. The client reconstructs the canonical tick-zero manifest from all coherent
   platform declarations and the received match options. It accepts only exact
   manifest equality, including the complete three- or four-member roster; omission,
   mutation, or peer reassignment fails closed.
8. Reconnect reuses the committed manifest's prior peer/seat ownership; it never
   rebuilds or reshuffles the roster during a fight.

Lobby chat is not a gameplay or roster data plane. The application must use the
known member-metadata key and exact codec, never arbitrary lobby key enumeration.
