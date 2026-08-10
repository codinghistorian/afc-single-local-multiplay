#!/bin/bash
set -euo pipefail

launcher_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$launcher_dir/../../.." && pwd)"
entitlements="$launcher_dir/steam-overlay.entitlements"
target_root="$repository_root/target/spacewar-macos"
binary="$target_root/debug/ffc-prototype"
runtime_steam_api="$target_root/debug/libsteam_api.dylib"
steam_overlay_candidates=(
    "$HOME/Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/gameoverlayrenderer.dylib"
    "/Applications/Steam.app/Contents/MacOS/gameoverlayrenderer.dylib"
)
overlay_renderer=""

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required to build the macOS Spacewar client." >&2
    exit 1
fi
if ! command -v codesign >/dev/null 2>&1; then
    echo "codesign is required to prepare the macOS Steam Overlay client." >&2
    exit 1
fi

for candidate in "${steam_overlay_candidates[@]}"; do
    if [[ -f "$candidate" ]]; then
        overlay_renderer="$candidate"
        break
    fi
done
if [[ -z "$overlay_renderer" ]]; then
    echo "Steam's macOS overlay renderer was not found. Install and start the Steam desktop client first." >&2
    exit 1
fi
overlay_signature="$(codesign -dv --verbose=4 "$overlay_renderer" 2>&1)"
if [[ "$overlay_signature" != *"Identifier=com.valvesoftware.steam"* \
    || "$overlay_signature" != *"TeamIdentifier=MXGJJ98X76"* ]]; then
    echo "Refusing to inject an overlay renderer that is not signed by Valve." >&2
    exit 1
fi

cd "$repository_root"
CARGO_TARGET_DIR="$target_root" AFC_STEAM_APP_ID=480 \
    cargo build --locked --no-default-features \
    --features native,steam-net,spacewar-dev --bin ffc-prototype

steam_api_candidates=(
    "$target_root"/debug/build/steamworks-sys-*/out/libsteam_api.dylib
)
if [[ ${#steam_api_candidates[@]} -ne 1 || ! -f "${steam_api_candidates[0]}" ]]; then
    echo "Expected exactly one built libsteam_api.dylib for the debug client." >&2
    exit 1
fi
cp -f "${steam_api_candidates[0]}" "$runtime_steam_api"
codesign --verify --strict "$runtime_steam_api"

codesign --force --sign - --entitlements "$entitlements" "$binary"
codesign --verify --strict "$binary"

BEVY_ASSET_ROOT="$repository_root" \
    AFC_STEAM_APP_ID=480 \
    AFC_STEAM_DEV_SPACEWAR_480=1 \
    DYLD_INSERT_LIBRARIES="$overlay_renderer" \
    exec "$binary" "$@"
