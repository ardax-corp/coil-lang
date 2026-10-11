#!/usr/bin/env bash
# Play or CI-drive the adventure demo.
#
#   ./demo.sh           # interactive on a TTY; otherwise read stdin under timeout
#   ./demo.sh --ci      # pipe transcript.txt under timeout
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
TIMEOUT_SECS="${TIMEOUT_SECS:-10}"
# coil reads no coil.toml: pass the project and stdlib roots as flags.
ROOTS=(--root "$HERE/src" --root "$ROOT/.deps/coil-stdlib/src" --root "$ROOT/../coil-stdlib/src")
ENTRY="$HERE/src/main.hy"

if [[ ! -x "$BIN" ]]; then
  echo "Building release coil…"
  cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

rm -f "$ROOT/out.hyc" "$HERE/out.hyc"

case "${1:-}" in
  --help | -h)
    cat <<'EOF'
Usage: demo.sh [--ci]

  (no args, TTY)   interactive adventure (end with Ctrl+D)
  (no args, pipe)  read stdin under timeout
  --ci / -c        pipe transcript.txt under timeout
EOF
    ;;
  --ci | -c)
    timeout "${TIMEOUT_SECS}s" "$BIN" "${ROOTS[@]}" --allow-read --allow-write "$ENTRY" <"$HERE/transcript.txt"
    ;;
  "")
    if [[ -t 0 ]]; then
      exec "$BIN" "${ROOTS[@]}" --allow-read --allow-write "$ENTRY"
    else
      timeout "${TIMEOUT_SECS}s" "$BIN" "${ROOTS[@]}" --allow-read --allow-write "$ENTRY"
    fi
    ;;
  *)
    echo "Unknown option: $1 (try --help)" >&2
    exit 2
    ;;
esac
