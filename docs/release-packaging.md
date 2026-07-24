# Native release packaging

This document is the operating contract for producing auditable Animal Fighter
Club release candidates for Steam. The release tooling stages and verifies
already-built native binaries; it does not build code, sign binaries, invoke
SteamCMD, upload depots, or make a Steam branch live.

The source of truth is
[`packaging/release-policy.json`](../packaging/release-policy.json). The portable
implementation is `scripts/release.py` plus `scripts/release_lib.py`. It uses
only the Python standard library and is exercised by synthetic fixtures that do
not require Cargo or the Steamworks SDK.

## Security and release boundary

A candidate is accepted only when all of the following are true:

- The Git worktree and recursive submodules exactly match one committed revision.
- The binary reports the expected shipping release identity.
- The compiled Steam App ID is the real product App ID, never Spacewar App ID 480.
- Windows, Linux, and macOS report byte-for-byte identical release identities and
  the same full source commit.
- Runtime assets are tracked, materialized, non-empty regular files.
- Build-time embedded data is absent from the depot payload.
- The platform binary and Steam API redistributable have the expected executable
  format and architecture.
- The sealed manifest, file set, sizes, executable flags, and SHA-256 values all
  agree.
- No `steam_appid.txt`, debug symbols, dedicated-server executable, `web_dist`,
  symlink, unlisted file, or Git LFS pointer is present.

There is no clean-tree bypass. A dirty local checkout is a development build, not
a release candidate. Commit the intended source changes and run the release from
that exact commit.

The release-candidate workflow produces unsigned internal candidates. Production
Windows signing and macOS signing/notarization must happen before `stage`; signing,
stapling, or otherwise modifying a sealed stage invalidates its manifest. Add a
dedicated pre-stage signing job before calling a candidate production-ready.

## Required release configuration

Configure the protected GitHub environment named `steam-release` with these
non-secret environment variables:

| Variable | Meaning |
| --- | --- |
| `AFC_STEAM_APP_ID` | Product Steam App ID compiled into every native binary |
| `AFC_STEAM_DEPOT_WINDOWS_ID` | Windows player depot |
| `AFC_STEAM_DEPOT_LINUX_ID` | Linux player depot |
| `AFC_STEAM_DEPOT_MACOS_ID` | macOS player depot |
| `AFC_MACOS_BUNDLE_ID` | Three-or-more-part reverse-DNS bundle ID |
| `AFC_MACOS_BUNDLE_VERSION` | One to three dot-separated integers |
| `AFC_MACOS_MIN_VERSION` | One to three dot-separated integers |

IDs must be non-zero decimal 32-bit values. App and depot IDs must all differ.
Release labels must match `[A-Za-z0-9][A-Za-z0-9._+-]{0,63}`. A useful immutable
label is `steam-rc.<sequence>+<12-character-commit>`.

Do not add Steam credentials to these variables, to the repository, to generated
VDF files, or to candidate artifacts. Upload credentials are outside this
repository's trust boundary.

## Build contract

Build the player binary from the audited source commit with the exact shipping
feature selection:

```sh
AFC_BUILD_ID="$RELEASE_LABEL" \
AFC_STEAM_APP_ID="$AFC_STEAM_APP_ID" \
CARGO_TARGET_DIR="$TARGET_DIR" \
cargo build --locked --release --no-default-features --features shipping \
  --bin ffc-prototype
```

Do not add `steam-dev`, `web`, or another feature. Do not build with the default
feature set. The candidate tool starts the binary with
`--release-identity` in a sanitized environment and rejects any build that
reports `shipping: false`, App ID 480, an unexpected label, or an incompatible
identity schema.

The shipping binary is also expected to report:

- product and package identity;
- product version;
- protocol, simulation, RNG, replay, and snapshot schema versions;
- compatibility build ID;
- gameplay-content hash;
- null Steam depot build ID before the external upload occurs.

Depot build IDs assigned later by Steam are acceptance-record metadata. They do
not modify a sealed artifact or its pre-upload release identity.

## Build hosts

### Windows x86-64

Build on the pinned Windows GitHub runner. The packager verifies PE x86-64 for
both `ffc-prototype.exe` and `steam_api64.dll`.

### Linux x86-64

Build inside the Steam Linux Runtime 4 SDK image pinned in the policy:

```text
registry.gitlab.steamos.cloud/steamrt/steamrt4/sdk:4.0.20260714.251823
sha256:2c4c6520a268ef53255d511ae5988e35855b39a4b6c1e9865d56e5011c76ec3e
```

The runtime's Steam App ID is **4183110**. This is external Steamworks Partner
runtime configuration, not an `AFC_STEAM_APP_ID`, Cargo feature, linker input, or
value compiled into the game. Configure the Linux launch option to use Steam
Linux Runtime 4 in Steamworks Partner. Valve's primary runtime documentation
maps `steamrt4` to App ID 4183110 and recommends it for newer native games:
[Valve Steam Linux Runtime reporting guide][valve-runtime].

The Linux depot entrypoint is `afc-launch`. It resolves its own directory,
prepends that directory to `LD_LIBRARY_PATH`, and executes `ffc-prototype`
without changing the working directory.

### macOS universal

Build `x86_64-apple-darwin` and `aarch64-apple-darwin` from the same commit and
release inputs, then combine the two player binaries with `lipo -create`.
The packager rejects a Mach-O that lacks either x86-64 or arm64. The Steam API
dylib is copied into `Contents/MacOS`, and the generated `Info.plist` uses the
validated bundle ID, build version, minimum OS version, and numeric product
version from the binary identity.

## Steam API redistributables

The native Steam API libraries are the redistributables emitted by the
`steamworks-sys` Cargo build:

- Windows: `steam_api64.dll`
- Linux: `libsteam_api.so`
- macOS: `libsteam_api.dylib`

Find exactly one matching file below a target build directory:

```sh
python3 scripts/release.py find-redistributable \
  --platform linux-x86_64 \
  --search-root "$TARGET_DIR/release/build"
```

The command only accepts a matching file under a
`steamworks-sys-*/out/` directory and validates its binary architecture.
Zero or multiple matches fail closed. In particular, the macOS
`libsteam_api.dylib` must itself contain x86-64 and arm64 slices; the candidate
workflow fails before staging if the SDK redistributable is not universal.
Valve documents that these are required Steam API redistributable binaries:
[Steamworks API overview][valve-api].

Never ship `steam_appid.txt`; it is a local development aid rather than a depot
file.

## Runtime asset contract

The source audit obtains the asset list from `git ls-files`, not from an
unbounded filesystem walk. Every tracked file below `assets/` is staged except
the explicit build-time embedded list in the release policy. This preserves
licenses and data files alongside models and Steam Input configuration.

The following are mandatory runtime files:

```text
assets/steam_input/action_manifest.vdf
assets/steam_input/generic_gamepad_default.vdf
assets/steam_input/steam_deck_default.vdf
```

RON sources embedded into the executable by `build.rs` are compile inputs only.
The policy enumerates the champions court, camera, character move-set, combat
feel, and arena-overlay RON files. They must not be duplicated in a depot.

When adding an embedded input, update `embedded_build_only_paths`. When adding a
mandatory runtime input, update `required_runtime_assets`. Unknown policy fields
are rejected so a typo cannot silently weaken the contract.

## Candidate workflow

Run the synthetic packaging tests on any supported host:

```sh
python3 scripts/release.py self-test -v
```

On Windows, use `python` if `python3` is not installed. The tests construct small
synthetic PE, ELF, and universal Mach-O fixtures. They do not build the game or
copy proprietary SDK files.

Before any build, audit the exact source:

```sh
python3 scripts/release.py audit-source
SOURCE_COMMIT="$(git rev-parse --verify HEAD)"
```

Validate all external inputs before allocating build time:

```sh
python3 scripts/release.py validate-inputs \
  --release-label "$RELEASE_LABEL" \
  --app-id "$AFC_STEAM_APP_ID" \
  --windows-depot-id "$AFC_STEAM_DEPOT_WINDOWS_ID" \
  --linux-depot-id "$AFC_STEAM_DEPOT_LINUX_ID" \
  --macos-depot-id "$AFC_STEAM_DEPOT_MACOS_ID" \
  --macos-bundle-id "$AFC_MACOS_BUNDLE_ID" \
  --macos-bundle-version "$AFC_MACOS_BUNDLE_VERSION" \
  --macos-min-version "$AFC_MACOS_MIN_VERSION"
```

Stage a built binary. This example uses Linux; choose the matching platform and
binary on the other hosts:

```sh
python3 scripts/release.py stage \
  --platform linux-x86_64 \
  --binary "$TARGET_DIR/release/ffc-prototype" \
  --redistributable-search-root "$TARGET_DIR/release/build" \
  --output "dist/stage/linux-x86_64" \
  --release-label "$RELEASE_LABEL" \
  --app-id "$AFC_STEAM_APP_ID"
```

For `macos-universal`, also pass:

```text
--macos-bundle-id <id>
--macos-bundle-version <version>
--macos-min-version <version>
```

`stage` audits the clean source again, validates inputs, executes the binary's
identity query, copies into a temporary sibling directory, seals the result, runs
a complete verification, and only then atomically publishes the stage. The
destination must not already exist.

Verify without Git, Cargo, or Steam:

```sh
python3 scripts/release.py verify \
  --stage "dist/stage/linux-x86_64" \
  --platform linux-x86_64 \
  --release-label "$RELEASE_LABEL" \
  --app-id "$AFC_STEAM_APP_ID" \
  --source-commit "$SOURCE_COMMIT"
```

Archive only a verified stage:

```sh
python3 scripts/release.py archive \
  --stage "dist/stage/linux-x86_64" \
  --output "dist/archives/afc-$RELEASE_LABEL-linux-x86_64.zip"
```

The archive is an uncompressed deterministic ZIP rooted at the depot root. File
ordering, timestamps, and POSIX modes are normalized. A sibling `.zip.sha256`
file authenticates the archive. A pre-existing output is never overwritten.

After all platforms exist, compare their manifests or stage directories:

```sh
python3 scripts/release.py compare-identities \
  dist/stage/windows-x86_64 \
  dist/stage/linux-x86_64 \
  dist/stage/macos-universal
```

The command requires distinct platforms, one source commit, and one exact
release identity. A manifest file is sufficient for cross-runner comparison;
passing a stage directory additionally verifies every payload byte.

## Sealed depot layouts

Windows depot root:

```text
ffc-prototype.exe
steam_api64.dll
assets/...
release-identity.json
release-manifest.json
SHA256SUMS
```

Linux depot root:

```text
afc-launch
ffc-prototype
libsteam_api.so
assets/...
release-identity.json
release-manifest.json
SHA256SUMS
```

macOS depot root:

```text
Animal Fighter Club.app/
  Contents/
    Info.plist
    MacOS/
      ffc-prototype
      libsteam_api.dylib
      assets/...
release-identity.json
release-manifest.json
SHA256SUMS
```

`release-manifest.json` contains the platform, full source commit, entrypoint,
exact release identity, Steam API library hash, Linux runtime declaration where
applicable, and the strictly sorted payload inventory. `SHA256SUMS` covers the
entire sealed payload plus the manifest. Any extra, missing, renamed, or modified
file invalidates verification.

Do not edit a sealed stage. Rebuild and restage from a clean commit.

## Preview-only SteamPipe files

Render SteamPipe input only after all three extracted stages pass verification:

```sh
python3 scripts/release.py render-steam-vdf \
  --release-label "$RELEASE_LABEL" \
  --source-commit "$SOURCE_COMMIT" \
  --app-id "$AFC_STEAM_APP_ID" \
  --windows-depot-id "$AFC_STEAM_DEPOT_WINDOWS_ID" \
  --linux-depot-id "$AFC_STEAM_DEPOT_LINUX_ID" \
  --macos-depot-id "$AFC_STEAM_DEPOT_MACOS_ID" \
  --windows-stage "dist/stage/windows-x86_64" \
  --linux-stage "dist/stage/linux-x86_64" \
  --macos-stage "dist/stage/macos-universal" \
  --build-output ".steam/build-output/$RELEASE_LABEL" \
  --output ".steam/vdf/$RELEASE_LABEL"
```

Both generated output directories must remain below the ignored `.steam/`
directory and must not overlap any depot content. Template values are escaped,
placeholder sets must match exactly, and rendered strings/braces are checked.
The app build always has `"Preview" "1"`. The renderer rejects upload commands,
credential fields, branch-live instructions, and unresolved placeholders.

This repository deliberately has no upload command. A separately authorized
release operator may inspect the preview files and use Valve's documented
SteamPipe process outside this repository: [Uploading to Steam][valve-upload].
Upload and making a branch live require distinct approval and external
credentials.

## CI artifacts and promotion record

`.github/workflows/ci.yml` validates the packaging tool on Windows, Linux, and
macOS, runs native and Steam-feature correctness gates, and writes the browser
build only to repository-root `web_dist/`.

`.github/workflows/cross-platform-determinism.yml` is configured to run the frozen
simulation tape, all 17 checked-in v5 behavior tapes, and the compact
authored-content matrix in both debug and release profiles on explicit Ubuntu
24.04, Windows 2025, and macOS 15 runners with Rust 1.94.1. A successful run from
the exact candidate is commit evidence; the workflow definition alone is not.
Neither substitutes for executing the sealed depot on physical target hardware.

`.github/workflows/release-candidate.yml` runs on explicit dispatch under the
protected `steam-release` environment. After preflight, one release-profile
acceptance job runs the production live-network matrix, explicit
security/reconnect cases, and the 100,000-tick production-Bevy repeated-hash
soak. All three native platform builds depend on that job. The workflow then
builds each platform from the same commit, stages and verifies it, creates a
deterministic archive, re-extracts and verifies the archives, compares the three
manifest identities, and renders preview-only SteamPipe evidence. The dispatch
form requires the operator to confirm the protected macOS configuration exactly.
It uploads candidates and evidence to GitHub Actions only. It never sends
content to Steam. The workflow definition is an available release path, not
evidence that a particular candidate run or any live Steam validation has
passed.

### Workflow dependency pinning

Every third-party GitHub Action in the CI, candidate, and cross-platform
determinism workflows must use a full 40-character commit SHA. The inline
comment retains the reviewed human-readable tag or branch. Resolve updates
directly from the action's official upstream repository with `git ls-remote`;
for an annotated tag, pin the peeled `refs/tags/<tag>^{}` commit rather than the
tag object. Review the upstream change before updating both the SHA and comment.
Floating action tags and branches are not accepted in `uses:`.

Release-evidence runner labels are explicit rather than `*-latest`, and Rust
workflows select toolchain `1.94.1`. Changing either is a reviewed release
configuration change and requires rerunning the full candidate evidence.

Before external Steam upload, record:

- workflow run URL and source commit;
- release label and exact release identity;
- the three archive names, byte sizes, and SHA-256 values;
- the three depot IDs and Steam Linux Runtime selection;
- signing/notarization evidence when production signing is introduced;
- operator and approver;
- preview result;
- Steam-assigned depot build IDs after upload;
- branch promotion approval and result.

Keep this record external to the sealed depot contents. A rejected preview,
failed upload, or changed depot input requires a new immutable release candidate.
Complete the account, network, controller, signing, and promotion evidence in
[Steam release acceptance](steam-release-acceptance.md); automated fake-platform
coverage and generated VDFs cannot satisfy those rows.

Native candidate artifacts live below `dist/`. Browser artifacts live below
`web_dist/`. Local SteamPipe working data lives below ignored `.steam/`. These
three trees are separate by design.

[valve-api]: https://partner.steamgames.com/doc/sdk/api
[valve-runtime]: https://gitlab.com/ValveSoftware/steam-runtime/-/blob/master/doc/reporting-steamlinuxruntime-bugs.md
[valve-upload]: https://partner.steamgames.com/doc/sdk/uploading
