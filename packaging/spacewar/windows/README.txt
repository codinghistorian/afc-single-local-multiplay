Animal Fighter Club - private Windows Spacewar test
===================================================

This is a development-only build for testing Steam multiplayer through Valve's
shared Spacewar App ID 480. It is not a Steam release or depot build.

Requirements
------------

1. Windows 10 or newer, 64-bit.
2. The Steam desktop client installed, running, and signed in.
3. A different Steam account from the other tester.
4. The two Steam accounts should be friends for friends-only lobby invitations.

Launch
------

1. Extract the complete ZIP. Do not move the EXE out of this folder.
2. Double-click run-spacewar.bat.
3. If Windows Firewall asks, allow the game on the network used for the test.
4. Press Shift+U to open the player-facing title flow.
5. Select Online and create or join a private/friends-only lobby.

Keep ffc-prototype.exe, steam_api64.dll, assets, and run-spacewar.bat together.
The launcher supplies guarded App ID 480 environment variables and starts the
game from the correct working directory. This package also carries a guarded,
compile-time Spacewar opt-in, so launching the EXE directly no longer produces
the "requires explicit opt-in" error. A steam_appid.txt file is intentionally
not included.

Both peers must use builds from the same source commit, Cargo profile, App ID,
and feature set. The matching macOS source launcher is:

  packaging/spacewar/macos/run-spacewar.sh

The macOS launcher builds both peers with the same Cargo features, initializes
Steam before Metal, and signs the local executable with Steam Overlay's required
development entitlements.

App ID 480 is shared by other developers. Use private or friends-only lobbies,
and distribute this package only to the intended testers.
