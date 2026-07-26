# Steam Online Release Acceptance Record

Use one copy of this record for each release candidate. Automated fake-platform
tests prove state-machine behavior; they do not count as Valve service, SDR, NAT,
overlay, account-license, or device evidence.

Do not record Steam IDs, IP addresses, ticket bytes, relay credentials, persona
names, or packet payloads. Identify accounts and machines only as `A`, `B`, and
so on. Keep the completed record with the private release artifacts. Follow
[Native release packaging](release-packaging.md) for the authoritative build,
stage, verification, archive, and preview-only SteamPipe contract.

## Candidate identity

| Field | Recorded value |
| --- | --- |
| Release-candidate workflow run | |
| Git commit | |
| UTC test date | |
| Immutable release label | |
| AFC Steam App ID (not 480) | |
| Configured Windows / Linux / macOS depot IDs | |
| Windows archive / SHA-256 | |
| Linux archive / SHA-256 | |
| macOS archive / SHA-256 | |
| Rust toolchain | |
| Protocol / simulation / RNG / replay / snapshot versions | |
| Compatibility build ID | |
| Gameplay-content hash | |
| Exact cross-platform `release-identity.json` | |
| Steam Linux Runtime 4 policy and Partner selection | |
| Signing / notarization evidence or explicit unsigned status | |
| Steam client versions | |

Attach each `release-manifest.json`, archive `.sha256` sidecar, and the
cross-platform identity-comparison output. Any binary, content, manifest,
signature, App/depot configuration, release identity, or archive change
invalidates the record and requires a new candidate run. Signing or notarizing
after `stage` changes the sealed payload and is not allowed; production signing
must occur before a new stage is sealed.

## Automated candidate gates

Attach command output and mark every row. Focused tests never replace the two
repository-required commands `cargo test` and `cargo run`.

| Gate | Result / artifact |
| --- | --- |
| `cargo fmt --all -- --check` and `git diff --check` | |
| `cargo check` and `cargo test` | |
| Normal-exit `cargo run` | |
| `python3 scripts/release.py self-test -v` | |
| Clean committed source passes `python3 scripts/release.py audit-source` | |
| `cargo check --locked --no-default-features --features native,steam-net` and matching `cargo test` | |
| Every player binary was built with `AFC_BUILD_ID=<label> AFC_STEAM_APP_ID=<real> cargo build --locked --release --no-default-features --features shipping --bin ffc-prototype` | |
| Windows, SteamRT4 Linux, and universal macOS pass `release.py stage`, `verify`, and `archive` | |
| Re-extracted archives pass full verification and `compare-identities` reports one exact identity and source commit | |
| Linux manifest records the policy-pinned SteamRT4 SDK tag/digest and runtime App ID 4183110 | |
| Generated SteamPipe VDF evidence is preview-only and contains no credentials, upload command, or branch-live instruction | |
| `cargo run --bin afc-dedicated -- --smoke-ticks 120` | |
| Release-profile network/security/reconnect acceptance job | |
| Release-profile 100,000-tick production-Bevy repeated-hash soak | |
| Linux/Windows/macOS debug+release frozen tape, all 17 checked-in behavior tapes, and authored-content workflow | |
| `./scripts/build_web.sh` with output only in `web_dist/` | |
| Default build shows Online unavailable but local/offline remains usable | |
| Each native archive contains only the manifested player payload, matching Steam API redistributable, and no `steam_appid.txt` or debug symbols | |

The frozen-tape workflow from the exact candidate commit must report the hashes
and team-1 result in
[cross-platform-determinism.md](cross-platform-determinism.md). It is build/test
evidence, not proof that the sealed player archive ran on Steam Deck; the
controller-only and match-lifecycle rows below provide that separate physical
evidence.

## External packaging and promotion gates

The repository and release-candidate workflow deliberately stop before
production distribution. Record every external step:

| Gate | Required observation | Result / artifact |
| --- | --- | --- |
| Protected IDs | The approved product App ID and three distinct depot IDs equal the values compiled/validated for this candidate. | |
| Windows signing | The signed executable was staged and sealed afterward; signature verification succeeds on a clean Windows machine. | |
| macOS signing/notarization | The signed/notarized universal application was staged and sealed afterward; Gatekeeper verification succeeds on a clean supported macOS machine. | |
| Linux runtime | Steamworks Partner selects Steam Linux Runtime 4 (`steamrt4`, runtime App ID 4183110) for the Linux launch option. This is not AFC's product App ID. | |
| Generated VDF review | The checked VDF artifact has `"Preview" "1"` and maps each verified stage to its approved depot ID. | |
| External SteamPipe preview | An authorized operator runs Valve's preview outside this repository and records the result without storing credentials here. | |
| Depot upload | The authorized external upload uses the exact sealed archive contents; record Steam's assigned build/depot IDs. | |
| Branch promotion | A distinct approval makes the intended Steam branch live only after every gate in this record passes. | |

Generating VDF files does not satisfy the external preview, upload, or branch
promotion rows. The current automated workflow produces unsigned internal
candidates unless a reviewed pre-stage signing integration is added.

## Physical test matrix

Use two licensed accounts on separate machines for every required host/client
pair. At least one run must place the peers behind different consumer NATs; at
least one must use a restrictive firewall or hotspot path.

| Run | Host OS/device | Client OS/device | Network path | Controllers | Pass/fail |
| --- | --- | --- | --- | --- | --- |
| Windows ↔ Windows | | | | | |
| Windows ↔ Linux | | | | | |
| Windows ↔ Steam Deck | | | | | |
| macOS ↔ Windows | | | | | |
| Cross-region | | | | | |

For each row, capture the Steam Networking Sockets connection status showing a
relay/SDR route. A direct public-IP path is not SDR acceptance. Store only the
route class, region, quality counters, and timestamps—not addresses.

## End-to-end scenarios

Run each scenario from a cold process start unless the row says otherwise.

| Scenario | Required observation | Pass/fail / evidence |
| --- | --- | --- |
| Controller-only cold boot | Steam Deck controls reach Online, create/join, lobby editing, fight, result, rematch, and menu without keyboard/pointer. | |
| Private lobby | Owner creates; an uninvited account cannot discover/join; invited peer joins. | |
| Friends-only lobby | Friend joins through overlay; a non-friend fails closed. | |
| Invite launch | Closed client accepts invite and boots through the exact lobby intent. | |
| Steam process bootstrap | Outside-Steam release launch requests relaunch and exits immediately; the replacement depot process reaches the title without custom AFC environment variables. | |
| `+connect_lobby` | Launch parameter is consumed once and reaches the same join flow. | |
| Lobby ownership and manifest | The current Steam-confirmed owner at between-match commit is authority; all peers agree on immutable manifest/content hash before loading. | |
| Authentication and ownership | Licensed accounts admit; wrong App ID, license failure, revocation, and banned account produce sanitized typed failures. | |
| Couch seats | One peer owns at least two seats; loadouts/teams/readiness agree on both machines. | |
| Match lifecycle | Countdown, prediction, rollback corrections, result confirmation, rematch, and return-to-lobby agree. | |
| Stable controller assignment | Four controllers retain P1–P4 assignment across enumeration reorder and reconnect. | |
| Rebinding | Steam layout panel rebinds gameplay/menu actions without changing protocol input shape. | |
| Network loss and reclaim | Disconnect, cable loss, suspend/resume, and same-account reclaim stay within grace and perform bounded resync. | |
| Grace expiry | Neutral substitution becomes deterministic bot takeover; stale identity cannot reclaim. | |
| Listen host loss | Remaining client receives no-contest and no progression, stays in the same Steam lobby with the transferred owner/role, and can start a fresh authority only after Return-to-Lobby; no mid-match migration is offered. | |
| Malformed/abusive peer | Only the offending peer is isolated; authority and other peer remain bounded. | |
| Overlay unavailable | Invite/binding-panel request shows the sanitized four-second dismissible notice without entering Error or blocking callback/input pumping. | |
| Clean teardown | Rematch and final process exit close tickets, sessions, endpoints, workers, and replay/diagnostic writers once. | |

## Performance and soak evidence

Use the same hardware and procedure in [performance.md](performance.md).

| Evidence | Result / artifact |
| --- | --- |
| Three final schema-v6 timing `FourBotStress` runs | Local matrix passes 3/3 on frozen patched binary SHA-256 `9caaa991644f367d772e11a4f7964ec71c25f0b51d496828558b1e2aaed6e7fd`; attach the sealed candidate's per-run host/power records here. |
| Three final schema-v6 allocator `FourBotStress` runs | Local matrix passes 3/3 on frozen patched binary SHA-256 `54d6239ec592bf3139f24cfc120abb23ccfbd7115a22e70bec097d7920b49db6`; attach the sealed candidate's per-run host/power records here. |
| Three final schema-v6 timing `MapCycle100` runs | Local matrix passes 3/3 with exactly 101 supported assets preloaded, 10 warm presents, 100 measured switches, 111 present ACKs, and 11 aligned checkpoints per run. Each run passes the aligned-tail RSS gates (range at most 8 MiB; slope at most 2 MiB/min), verifies timing binary SHA-256 `9caaa991644f367d772e11a4f7964ec71c25f0b51d496828558b1e2aaed6e7fd` before/after, and retains before/after host and AC-power records. Attach the sealed-candidate records here. |
| Three final schema-v6 allocator `MapCycle100` runs | Local matrix passes 3/3 with exactly 101 supported assets preloaded, 10 warm presents, 100 measured switches, 111 present ACKs, and 11 aligned checkpoints per run. Each accepted run passes the aligned-tail RSS gates (range at most 8 MiB; slope at most 2 MiB/min) and live-allocation gates (range at most 1 MiB; slope at most 0.25 MiB/min), verifies allocator binary SHA-256 `54d6239ec592bf3139f24cfc120abb23ccfbd7115a22e70bec097d7920b49db6` before/after, and retains before/after host and AC-power records. One otherwise passing AC-to-battery sample was rejected and replaced, so it is excluded from the 3/3 result. Attach the sealed-candidate records here. |
| One final schema-v6 timing `Soak10Minutes` run | Local matrix passes 1/1 on the frozen patched timing binary; attach the sealed-candidate result and per-run host/power records here. |
| One final schema-v6 allocator `Soak10Minutes` run with RSS and live-allocation plateau analysis | Local matrix passes 1/1 on the frozen patched allocator binary; attach the sealed-candidate result and per-run host/power records here. |
| Authority and exact 12-tick rollback p50/p95/p99/max and over-budget ticks | Local Apple M2 Max verification (`final-local-01` through `03`) passed all three runs. Authority p50/p95/p99/max ranges were 53,833–55,625 / 64,417–70,416 / 66,916–73,166 / 71,333–89,292 ns, with zero samples over the 1 ms budget and zero steady-state allocations. Rollback p50/p95/p99/max ranges were 356,000–356,792 / 363,833–365,958 / 370,417–376,958 / 385,625–418,667 ns, with zero samples over the 4 ms budget. Attach the sealed-candidate and minimum-supported-CPU records separately. |
| Minimum-supported-CPU authority and rollback budgets | Pending; the local Apple M2 Max matrix is not minimum-supported-CPU approval. |
| External GPU capture for the graphical stress and soak workloads | Pending; every local schema-v6 result reports `external_gpu_evidence_status=required_not_collected` and `gpu_completion_measured=false`. |
| Cross-region Steam soak: duration, reconnects, hard resyncs, bandwidth, queue high-water | |
| Incident/replay retention and privacy review | |

## Decision

Release approval requires every automated gate, physical scenario, performance
review, privacy review, external packaging gate, and promotion approval to pass
on the exact sealed candidate. Public lobbies, hosted dedicated admission,
ranked/trusted results, spectators, mid-match join, and host migration must
remain unavailable. Spacewar App ID 480 is development evidence only and can
never approve a shipping candidate.

| Role | Name | Decision | Date |
| --- | --- | --- | --- |
| Engineering | | | |
| QA | | | |
| Security/privacy | | | |
| Release owner | | | |
