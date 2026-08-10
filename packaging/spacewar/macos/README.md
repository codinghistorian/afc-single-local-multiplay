# macOS Spacewar test launcher

Start and sign in to the Steam desktop client first, then run this command from
the repository root:

```sh
packaging/spacewar/macos/run-spacewar.sh
```

The launcher builds an App ID 480 debug client with the same Cargo feature set
as the Windows test artifact, applies Valve's macOS Steam Overlay development
entitlements, stages Valve's signed `libsteam_api.dylib`, verifies both
signatures, points Bevy at the repository assets, and starts the game. Keep
Steam Overlay enabled globally and for Spacewar.

The dedicated build is kept under `target/spacewar-macos/`, separate from
ordinary `cargo run` output. The first build can take a few minutes; later
launches reuse it.

If the in-game invite overlay still does not attach, use Steam Friends & Chat:
the second account can right-click the lobby host and select **Join Game** while
the host's private lobby is open.

This launcher and App ID 480 are development-only. They are rejected by the
project's release/shipping build checks.
