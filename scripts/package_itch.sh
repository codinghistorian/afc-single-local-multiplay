#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/web_dist"
STAGING_DIR="$ROOT_DIR/target/itch"
ARCHIVE_NAME="animal-fighter-club-web.zip"
ARCHIVE="$DIST_DIR/$ARCHIVE_NAME"
TEMP_ARCHIVE="$STAGING_DIR/.${ARCHIVE_NAME}.tmp"
MAX_FILE_COUNT=1000
MAX_EXTRACTED_BYTES=500000000
MAX_INDIVIDUAL_BYTES=200000000

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required tool '$1' was not found." >&2
    exit 1
  fi
}

format_bytes() {
  awk -v bytes="$1" 'BEGIN { printf "%.2f MiB", bytes / 1048576 }'
}

require_tool zip
require_tool unzip

if [[ ! -f "$DIST_DIR/index.html" ]]; then
  echo "Missing $DIST_DIR/index.html. Run ./scripts/build_web.sh first." >&2
  exit 1
fi
if [[ ! -d "$DIST_DIR/pkg" || ! -d "$DIST_DIR/assets" ]]; then
  echo "web_dist must contain pkg/ and assets/ beside index.html." >&2
  exit 1
fi

file_count=0
extracted_bytes=0
largest_bytes=0
largest_file=""
while IFS= read -r -d '' file; do
  bytes="$(wc -c < "$file" | tr -d '[:space:]')"
  relative="${file#"$DIST_DIR"/}"
  file_count=$((file_count + 1))
  extracted_bytes=$((extracted_bytes + bytes))
  if (( bytes > largest_bytes )); then
    largest_bytes="$bytes"
    largest_file="$relative"
  fi
  if (( bytes > MAX_INDIVIDUAL_BYTES )); then
    echo "$relative is $(format_bytes "$bytes"), above itch.io's 200 MB per-file limit." >&2
    exit 1
  fi
done < <(find "$DIST_DIR" -type f ! -path "$ARCHIVE" -print0)

if (( file_count >= MAX_FILE_COUNT )); then
  echo "web_dist contains $file_count files; itch.io requires fewer than $MAX_FILE_COUNT." >&2
  exit 1
fi
if (( extracted_bytes >= MAX_EXTRACTED_BYTES )); then
  echo "web_dist extracts to $(format_bytes "$extracted_bytes"); itch.io requires less than 500 MB." >&2
  exit 1
fi

mkdir -p "$STAGING_DIR"
rm -f "$TEMP_ARCHIVE"
(
  cd "$DIST_DIR"
  zip -q -r "$TEMP_ARCHIVE" . -x "$ARCHIVE_NAME" "./$ARCHIVE_NAME"
)

if ! unzip -Z1 "$TEMP_ARCHIVE" | awk '$0 == "index.html" { found=1 } END { exit found ? 0 : 1 }'; then
  echo "Archive validation failed: index.html is not at the ZIP root." >&2
  exit 1
fi
if unzip -Z1 "$TEMP_ARCHIVE" | awk '/^web_dist\// { found=1 } END { exit found ? 0 : 1 }'; then
  echo "Archive validation failed: web_dist/ must not wrap the game files." >&2
  exit 1
fi
unzip -tq "$TEMP_ARCHIVE" >/dev/null
mv -f "$TEMP_ARCHIVE" "$ARCHIVE"

archive_bytes="$(wc -c < "$ARCHIVE" | tr -d '[:space:]')"
echo "itch.io package: $file_count files, $(format_bytes "$extracted_bytes") extracted."
echo "Largest file: $largest_file ($(format_bytes "$largest_bytes"))."
echo "Archive: $ARCHIVE ($(format_bytes "$archive_bytes"))."
