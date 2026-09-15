#!/usr/bin/env bash
# Soft CPU baseline for stack-IL / opt changes (see AGENTS.md).
#
# Not a hard CI gate: prints poop stats for the fair cross-lang perf subset so
# regressions are easy to spot before/after compiler changes. Requires `poop`.
# Times `coil run` on a precompiled archive (not in-memory compile+run).
# Flagship mandelbrot/nsieve/tak/binary_trees do not emit IPA; fib is compiled
# with COIL_AUTO_PAR=0 so this row stays sequential. For IPA vs seq counters
# see docs/internals/auto-par-branch-misses.md (COIL_PAR_STATS=1).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
DURATION_MS="${DURATION_MS:-6000}"
OUT_DIR="${OUT_DIR:-/tmp/coil_poop_baseline}"

BENCHES=(
    examples/perf/mandelbrot.hy
    examples/perf/tak.hy
    examples/perf/nsieve.hy
    examples/perf/binary_trees.hy
    examples/perf/fib.hy
)

if ! command -v poop >/dev/null 2>&1; then
    echo "poop not installed; install it or skip this baseline" >&2
    exit 1
fi

echo "== Building release binary =="
RUSTC_WRAPPER= cargo build --release --quiet

mkdir -p "$OUT_DIR"
for path in "${BENCHES[@]}"; do
    name="$(basename "$path" .hy)"
    hyc="$OUT_DIR/${name}.hyc"
    echo "== compile ${path} -> ${hyc} =="
    if [[ "$name" == "fib" ]]; then
        COIL_AUTO_PAR=0 "$BIN" compile "$path" -o "$hyc" >/dev/null
    else
        "$BIN" compile "$path" -o "$hyc" >/dev/null
    fi
    echo "== poop -d ${DURATION_MS} coil run ${name}.hyc =="
    poop -d "$DURATION_MS" "$BIN run $hyc"
done
