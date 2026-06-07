#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="web_dist"
PKG_DIR="$OUT_DIR/pkg"
WASM_TARGET_DIR="target/wasm32-unknown-unknown/release"

rm -rf "$OUT_DIR"
mkdir -p "$PKG_DIR"

cargo build --release --target wasm32-unknown-unknown --no-default-features --features web

WASM_FILE="$(find "$WASM_TARGET_DIR" -maxdepth 1 -name '*.wasm' | head -n 1)"
if [[ -z "$WASM_FILE" ]]; then
  echo "No wasm file found in $WASM_TARGET_DIR" >&2
  exit 1
fi

wasm-bindgen \
  --out-name ffc_prototype \
  --out-dir "$PKG_DIR" \
  --target web \
  "$WASM_FILE"

BUILD_ID="$(date +%s)"
perl -0pi -e "s|new URL\\('ffc_prototype_bg\\.wasm', import\\.meta\\.url\\)|new URL('ffc_prototype_bg.wasm?v=$BUILD_ID', import.meta.url)|" "$PKG_DIR/ffc_prototype.js"

cp -R assets "$OUT_DIR/assets"
sed "s|./pkg/ffc_prototype.js|./pkg/ffc_prototype.js?v=$BUILD_ID|" web/index.html > "$OUT_DIR/index.html"

echo "Built $OUT_DIR. Serve with: python3 -m http.server 8000 --directory $OUT_DIR"
