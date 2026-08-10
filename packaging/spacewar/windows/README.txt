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
2. Double-click run-spacewar.bat. Do not launch ffc-prototype.exe directly.
3. If Windows Firewall asks, allow the game on the network used for the test.
4. Press Shift+U to open the player-facing title flow.
5. Select Online and create or join a private/friends-only lobby.

Keep ffc-prototype.exe, steam_api64.dll, assets, and run-spacewar.bat together.
The launcher supplies the two guarded App ID 480 environment variables required
by this project. A steam_appid.txt file is intentionally not included.

Both peers must use builds from the same source commit, Cargo profile, App ID,
and feature set. The matching macOS source command is:

  AFC_STEAM_APP_ID=480 AFC_STEAM_DEV_SPACEWAR_480=1 \
    cargo run --locked --no-default-features --features native,steam-net

App ID 480 is shared by other developers. Use private or friends-only lobbies,
and distribute this package only to the intended testers.
