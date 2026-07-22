#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="web_dist"
PKG_DIR="$OUT_DIR/pkg"
WASM_TARGET_DIR="target/wasm32-unknown-unknown/release"
WASM_SIZE_TARGET_BYTES="${WASM_SIZE_TARGET_BYTES:-61498982}"
WASM_SIZE_BUDGET_BYTES="${WASM_SIZE_BUDGET_BYTES:-64573931}"

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required tool '$1' was not found. $2" >&2
    exit 1
  fi
}

size_bytes() {
  wc -c < "$1" | tr -d '[:space:]'
}

format_bytes() {
  awk -v bytes="$1" 'BEGIN { printf "%.2f MiB", bytes / 1048576 }'
}

require_tool wasm-bindgen "Install it with: cargo install wasm-bindgen-cli --version 0.2.121"
require_tool wasm-opt "Install Binaryen (macOS: brew install binaryen)."

rm -rf "$OUT_DIR"
mkdir -p "$PKG_DIR"

cargo build --release --target wasm32-unknown-unknown --no-default-features --features web

WASM_FILE="$(find "$WASM_TARGET_DIR" -maxdepth 1 -name '*.wasm' | head -n 1)"
if [[ -z "$WASM_FILE" ]]; then
  echo "No wasm file found in $WASM_TARGET_DIR" >&2
  exit 1
fi

RAW_WASM_BYTES="$(size_bytes "$WASM_FILE")"

wasm-bindgen \
  --out-name ffc_prototype \
  --out-dir "$PKG_DIR" \
  --target web \
  "$WASM_FILE"

BINDGEN_WASM="$PKG_DIR/ffc_prototype_bg.wasm"
PRE_OPT_WASM_BYTES="$(size_bytes "$BINDGEN_WASM")"
OPTIMIZED_WASM="$BINDGEN_WASM.optimized"
wasm-opt -O3 --strip-debug "$BINDGEN_WASM" -o "$OPTIMIZED_WASM"
mv "$OPTIMIZED_WASM" "$BINDGEN_WASM"
FINAL_WASM_BYTES="$(size_bytes "$BINDGEN_WASM")"

if (( FINAL_WASM_BYTES > WASM_SIZE_BUDGET_BYTES )); then
  echo "Optimized WASM is $(format_bytes "$FINAL_WASM_BYTES"), above the $(format_bytes "$WASM_SIZE_BUDGET_BYTES") budget." >&2
  echo "Set WASM_SIZE_BUDGET_BYTES only when accepting and documenting a new measured baseline." >&2
  exit 1
fi

if (( FINAL_WASM_BYTES <= WASM_SIZE_TARGET_BYTES )); then
  SIZE_STATUS="target met"
else
  SIZE_STATUS="within guardrail; optimization target not yet met"
fi

BUILD_ID="$(date +%s)"
perl -0pi -e "s|new URL\\('ffc_prototype_bg\\.wasm', import\\.meta\\.url\\)|new URL('ffc_prototype_bg.wasm?v=$BUILD_ID', import.meta.url)|" "$PKG_DIR/ffc_prototype.js"

cp -R assets "$OUT_DIR/assets"
sed "s|./pkg/ffc_prototype.js|./pkg/ffc_prototype.js?v=$BUILD_ID|" web/index.html > "$OUT_DIR/index.html"

DIST_KIB="$(du -sk "$OUT_DIR" | awk '{print $1}')"
echo "WASM: raw $(format_bytes "$RAW_WASM_BYTES"), bindgen $(format_bytes "$PRE_OPT_WASM_BYTES"), optimized $(format_bytes "$FINAL_WASM_BYTES") ($SIZE_STATUS)."
echo "Static distribution: $(awk -v kib="$DIST_KIB" 'BEGIN { printf "%.2f MiB", kib / 1024 }')."
echo "Built $OUT_DIR. Serve with: python3 -m http.server 8000 --directory $OUT_DIR"
