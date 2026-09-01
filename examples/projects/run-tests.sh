#!/usr/bin/env bash
# Run co-located `coil test` suites for each showcase project.
# Harness only scans ./tests relative to CWD — hence the per-project cd.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
# Relative BIN is resolved from ROOT so per-project `cd` does not break it.
if [[ "$BIN" != /* ]]; then
  BIN="$ROOT/$BIN"
fi
TIMEOUT_SECS="${TIMEOUT_SECS:-60}"
PROJECTS="$ROOT/examples/projects"

if [[ ! -x "$BIN" ]]; then
  echo "Building release coil…"
  cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

# GNU `timeout` is absent on stock macOS; prefer gtimeout (coreutils) then bare run.
run_coil_test() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "${TIMEOUT_SECS}s" "$BIN" test \
      --root src \
      --root ../../../.deps/coil-stdlib/src \
      --root ../../../../coil-stdlib/src
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "${TIMEOUT_SECS}s" "$BIN" test \
      --root src \
      --root ../../../.deps/coil-stdlib/src \
      --root ../../../../coil-stdlib/src
  else
    "$BIN" test \
      --root src \
      --root ../../../.deps/coil-stdlib/src \
      --root ../../../../coil-stdlib/src
  fi
}

failed=0
for proj in 01-todo 02-adventure 03-echo; do
  echo "=== $proj tests ==="
  rm -f "$PROJECTS/$proj/out.hyc" "$ROOT/out.hyc"
  if (
    cd "$PROJECTS/$proj"
    run_coil_test
  ); then
    echo
  else
    echo "FAILED: $proj" >&2
    failed=1
    echo
  fi
done

if [[ "$failed" -ne 0 ]]; then
  echo "One or more showcase test suites failed." >&2
  exit 1
fi
echo "All showcase project tests passed."
