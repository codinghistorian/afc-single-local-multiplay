# macOS Spacewar test launcher

Start and sign in to the Steam desktop client first, then run this command from
the repository root:

```sh
packaging/spacewar/macos/run-spacewar.sh
```

The launcher builds an App ID 480 debug client with the same Cargo feature set
as the Windows test artifact, applies Valve's macOS Steam Overlay development
entitlements, stages Valve's signed `libsteam_api.dylib`, locates Steam's
Valve-signed `gameoverlayrenderer.dylib`, and injects that renderer before the
game initializes Steam or Metal. It also points Bevy at the repository assets
and starts the game. Keep Steam Overlay enabled globally and for Spacewar.

The dedicated build is kept under `target/spacewar-macos/`, separate from
ordinary `cargo run` output. The first build can take a few minutes; later
launches reuse it.

After creating a private lobby, select **Invite Friends**. The Steam invite
overlay should open; `Shift+Tab` also toggles the overlay. If it still does not
attach, use Steam Friends & Chat: the second account can right-click the lobby
host and select **Join Game** while the host's private lobby is open.

If an **Online Error** appears, capture the complete message and its
**Diagnostic code** from both computers. Numeric diagnostics are enabled only
in this guarded Spacewar test build; ordinary and shipping clients keep
internal detail hidden.

This launcher and App ID 480 are development-only. They are rejected by the
project's release/shipping build checks.
