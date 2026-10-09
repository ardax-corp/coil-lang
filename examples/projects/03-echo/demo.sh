#!/usr/bin/env bash
# Run the 03-echo TCP showcase demo (always under timeout).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
TIMEOUT_SECS="${TIMEOUT_SECS:-10}"

if [[ ! -x "$BIN" ]]; then
  echo "Building release coil…"
  cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

rm -f "$ROOT/out.hyc" "$HERE/out.hyc"
timeout "${TIMEOUT_SECS}s" "$BIN" --allow-net "$HERE/src/main.hy"
