#!/usr/bin/env bash
# VM measurement + correctness harness (Phase VM instruction-count reduction).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
MEM_LIMIT_KB="${MEM_LIMIT_KB:-65536}"

declare -A EXPECTED=(
    ["examples/fib.coil"]="2178309"
    ["examples/option.coil"]="42"
    ["examples/result.coil"]="420-1"
    ["examples/tree.coil"]="6"
    ["examples/mixed.coil"]=$'0\n25\n12\n2'
    ["examples/record.coil"]="169512"
    ["examples/let_test.coil"]="51020"
    ["examples/chained.coil"]="427"
    ["examples/dict.coil"]="4210042"
    ["examples/aliases.coil"]="347"
    ["examples/fizbuz.coil"]="FIZBUZFIZFIZBUZFIZFIZBUZ"
    ["examples/operators.coil"]="801125428falsetrue3"
    ["examples/perf/numeric.coil"]="1999000"
    ["examples/perf/array_mut.coil"]="2000"
    ["examples/perf/dict_hot.coil"]="6000"
    ["examples/perf/operators_loop.coil"]="149912"
    ["examples/perf/coro_ping.coil"]="124750"
    ["examples/perf/mandelbrot.coil"]="625885"
    ["examples/perf/tak.coil"]="7"
    ["examples/perf/nsieve.coil"]="1900"
    ["examples/perf/binary_trees.coil"]="135854"
    ["examples/perf/fib.coil"]="2178309"
    ["examples/perf/bool_guard.coil"]="45"
    ["examples/inline_wrapped_call.coil"]="13"
    ["examples/perf/gc_churn.coil"]="62499500000"
    ["examples/perf/option_int_churn.coil"]="84000000"
    ["examples/perf/result_int_churn.coil"]="38000000"
    ["examples/perf/result_heap_churn.coil"]="130000000"
    ["examples/perf/host_result_unit_churn.coil"]="18000000"
    ["examples/perf/result_try_churn.coil"]="136000000"
    ["examples/perf/pair_int_churn.coil"]="200000000"
    ["examples/perf/iv_mul_sr.coil"]="31499989500000"
    ["examples/perf/licm_nested_chains.coil"]="64012000000"
    ["examples/perf/licm_lir_pure_call.coil"]="157207000000"
    ["examples/perf/tail_sibling.coil"]="202500000"
    ["examples/perf/cse_index_recompute.coil"]="504000000"
    ["examples/perf/cse_cast_recompute.coil"]="1999998000000"
    ["examples/perf/dest_prop_field_alias.coil"]="40000000"
    ["examples/perf/nested_match.coil"]="11250000"
    ["examples/perf/int_match.coil"]="77470488"
)

# CPU-focused subset for poop / quick timing (no FFI, no modules).
CPU_BENCH=(
    examples/perf/mandelbrot.coil
    examples/perf/tak.coil
    examples/perf/nsieve.coil
    examples/perf/binary_trees.coil
    examples/perf/fib.coil
    examples/perf/numeric.coil
    examples/perf/operators_loop.coil
    examples/perf/match_sum.coil
    examples/perf/gc_churn.coil
    examples/perf/option_int_churn.coil
    examples/perf/result_int_churn.coil
    examples/perf/result_heap_churn.coil
    examples/perf/host_result_unit_churn.coil
    examples/perf/result_try_churn.coil
    examples/perf/pair_int_churn.coil
    examples/perf/iv_mul_sr.coil
    examples/perf/licm_nested_chains.coil
    examples/perf/licm_lir_pure_call.coil
    examples/perf/tail_sibling.coil
    examples/perf/cse_index_recompute.coil
    examples/perf/cse_cast_recompute.coil
    examples/perf/dest_prop_field_alias.coil
    examples/perf/nested_match.coil
    examples/perf/int_match.coil
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
    rm -f out.ccoil
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
        hyc="$POOP_DIR/${name}.ccoil"
        if [[ "$name" == "fib" ]]; then
            "$BIN" compile --no-jit --auto-par off "examples/perf/${name}.coil" -o "$hyc" >/dev/null
        else
            "$BIN" compile --no-jit "examples/perf/${name}.coil" -o "$hyc" >/dev/null
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
        name="$(basename "$path" .coil)"
        hyc="$POOP_DIR/${name}.ccoil"
        echo "-- $path"
        if [[ "$name" == "fib" ]]; then
            "$BIN" compile --no-jit --auto-par off "$path" -o "$hyc" >/dev/null
        else
            "$BIN" compile --no-jit "$path" -o "$hyc" >/dev/null
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
    hyc="$POOP_DIR/mandelbrot.ccoil"
    "$BIN" compile --no-jit examples/perf/mandelbrot.coil -o "$hyc" >/dev/null
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
