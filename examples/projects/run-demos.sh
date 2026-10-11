#!/usr/bin/env bash
# Run the three showcase demos under examples/projects/.
# Adventure is always driven from transcript.txt under `timeout`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
TIMEOUT_SECS="${TIMEOUT_SECS:-10}"
PROJECTS="$ROOT/examples/projects"
# coil reads no coil.toml: pass each project's and the stdlib roots as flags.
roots() {
  echo --root "$PROJECTS/$1/src" --root "$ROOT/.deps/coil-stdlib/src" --root "$ROOT/../coil-stdlib/src"
}

if [[ ! -x "$BIN" ]]; then
  echo "Building release coil…"
  cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

echo "=== 01-todo ==="
rm -f "$ROOT/out.hyc"
timeout "${TIMEOUT_SECS}s" "$BIN" $(roots 01-todo) "$PROJECTS/01-todo/src/main.hy"
echo

echo "=== 02-adventure (canned transcript) ==="
rm -f "$ROOT/out.hyc"
timeout "${TIMEOUT_SECS}s" "$BIN" $(roots 02-adventure) --allow-read --allow-write "$PROJECTS/02-adventure/src/main.hy" \
  <"$PROJECTS/02-adventure/transcript.txt"
echo

echo "=== 03-echo ==="
rm -f "$ROOT/out.hyc"
timeout "${TIMEOUT_SECS}s" "$BIN" $(roots 03-echo) --allow-net "$PROJECTS/03-echo/src/main.hy"
echo

echo "All showcase demos finished."
