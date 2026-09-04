#!/usr/bin/env bash
# VM measurement + correctness harness (Phase VM instruction-count reduction).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
MEM_LIMIT_KB="${MEM_LIMIT_KB:-65536}"

declare -A EXPECTED=(
    ["examples/fib.hy"]="55"
    ["examples/option.hy"]="42"
    ["examples/result.hy"]="420-1"
    ["examples/tree.hy"]="6"
    ["examples/mixed.hy"]="025122"
    ["examples/record.hy"]="169512"
    ["examples/let_test.hy"]="51020"
    ["examples/chained.hy"]="427"
    ["examples/dict.hy"]="4210042"
    ["examples/aliases.hy"]="347"
    ["examples/fizbuz.hy"]="FIZBUZFIZFIZBUZFIZFIZBUZ"
    ["examples/operators.hy"]="801125428falsetrue3"
    ["examples/perf/numeric.hy"]="1999000"
    ["examples/perf/array_mut.hy"]="2000"
    ["examples/perf/dict_hot.hy"]="6000"
    ["examples/perf/operators_loop.hy"]="149912"
    ["examples/perf/coro_ping.hy"]="124750"
    ["examples/perf/mandelbrot.hy"]="625885"
    ["examples/perf/tak.hy"]="7"
    ["examples/perf/nsieve.hy"]="1900"
    ["examples/perf/binary_trees.hy"]="135854"
    ["examples/perf/fib.hy"]="2178309"
    ["examples/perf/bool_guard.hy"]="45"
    ["examples/inline_wrapped_call.hy"]="13"
    ["examples/perf/gc_churn.hy"]="62499500000"
    ["examples/perf/option_int_churn.hy"]="84000000"
    ["examples/perf/result_int_churn.hy"]="38000000"
    ["examples/perf/result_heap_churn.hy"]="130000000"
    ["examples/perf/host_result_unit_churn.hy"]="18000000"
    ["examples/perf/result_try_churn.hy"]="136000000"
    ["examples/perf/pair_int_churn.hy"]="200000000"
)

# CPU-focused subset for poop / quick timing (no FFI, no modules).
CPU_BENCH=(
    examples/perf/mandelbrot.hy
    examples/perf/tak.hy
    examples/perf/nsieve.hy
    examples/perf/binary_trees.hy
    examples/perf/fib.hy
    examples/perf/numeric.hy
    examples/perf/operators_loop.hy
    examples/perf/match_sum.hy
    examples/perf/gc_churn.hy
    examples/perf/option_int_churn.hy
    examples/perf/result_int_churn.hy
    examples/perf/result_heap_churn.hy
    examples/perf/host_result_unit_churn.hy
    examples/perf/result_try_churn.hy
    examples/perf/pair_int_churn.hy
)

CROSS_LANG=(
    mandelbrot
    tak
    nsieve
    binary_trees
    fib
)

run_example() {
    local path="$1"
    rm -f out.hyc
    ulimit -v "$MEM_LIMIT_KB"
    "$BIN" "$path"
}

echo "== Building release binary =="
RUSTC_WRAPPER= cargo build --release

echo
echo "== Example stdout correctness (ulimit -v ${MEM_LIMIT_KB}) =="
fail=0
for path in "${!EXPECTED[@]}"; do
    want="${EXPECTED[$path]}"
    got="$(run_example "$path" 2>/dev/null || true)"
    if [[ "$got" == "$want" ]]; then
        echo "OK  $path -> $got"
    else
        echo "FAIL $path"
        echo "  expected: $want"
        echo "  got:      $got"
        fail=1
    fi
done

if [[ "$fail" -ne 0 ]]; then
    echo "Example correctness: FAILED"
    exit 1
fi
echo "Example correctness: PASSED"

if command -v poop >/dev/null 2>&1; then
    echo
    echo "== poop benchmark (coil run archive vs lua vs node) =="
    POOP_DIR="${POOP_DIR:-/tmp/coil_vm_bench}"
    mkdir -p "$POOP_DIR"
    for name in "${CROSS_LANG[@]}"; do
        echo "-- $name"
        hyc="$POOP_DIR/${name}.hyc"
        if [[ "$name" == "fib" ]]; then
            COIL_AUTO_PAR=0 "$BIN" compile "examples/perf/${name}.hy" -o "$hyc" >/dev/null
        else
            "$BIN" compile "examples/perf/${name}.hy" -o "$hyc" >/dev/null
        fi
        cmds=("$BIN run $hyc" "lua benchmarks/${name}.lua")
        if command -v node >/dev/null 2>&1; then
            cmds+=("node benchmarks/${name}.js")
        fi
        poop -d 6000 "${cmds[@]}" || true
    done
    echo
    echo "== poop CPU bench subset (precompiled) =="
    for path in "${CPU_BENCH[@]}"; do
        name="$(basename "$path" .hy)"
        hyc="$POOP_DIR/${name}.hyc"
        echo "-- $path"
        if [[ "$name" == "fib" ]]; then
            COIL_AUTO_PAR=0 "$BIN" compile "$path" -o "$hyc" >/dev/null
        else
            "$BIN" compile "$path" -o "$hyc" >/dev/null
        fi
        poop -d 3000 "$BIN run $hyc" || true
    done
else
    echo
    echo "poop not installed; skipping instruction-count benchmark"
fi

echo
echo "== Release binary size =="
ls -lh "$BIN"

if command -v valgrind >/dev/null 2>&1; then
    echo
    echo "== callgrind on mandelbrot (archive) =="
    POOP_DIR="${POOP_DIR:-/tmp/coil_vm_bench}"
    mkdir -p "$POOP_DIR"
    hyc="$POOP_DIR/mandelbrot.hyc"
    "$BIN" compile examples/perf/mandelbrot.hy -o "$hyc" >/dev/null
    rm -f callgrind.out.*
    valgrind --tool=callgrind --callgrind-out-file=callgrind.out "$BIN" run "$hyc" >/dev/null 2>&1 || true
    if command -v callgrind_annotate >/dev/null 2>&1; then
        callgrind_annotate callgrind.out 2>/dev/null | head -20 || true
    fi
else
    echo
    echo "valgrind not installed; skipping callgrind"
fi

echo
echo "Done."
