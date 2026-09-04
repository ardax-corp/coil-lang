#!/usr/bin/env bash
# Capture repeatable AOT and cross-language performance artifacts.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

BIN="${BIN:-$CARGO_TARGET_DIR/release/coil}"
DURATION_MS="${DURATION_MS:-6000}"
OUT_DIR="${OUT_DIR:-/tmp/coil_perf_matrix}"
RUN_MASSIF="${RUN_MASSIF:-0}"

CROSS_LANG=(mandelbrot tak nsieve binary_trees fib)
AOT_ONLY=(numeric operators_loop match_sum option_result field_hot dict_hot array_mut match_enum_loop nbody dict_count for_in_sum gc_churn option_int_churn result_int_churn result_heap_churn host_result_unit_churn result_try_churn pair_int_churn)
declare -A EXPECTED=(
    [mandelbrot]=625885
    [tak]=7
    [nsieve]=1900
    [binary_trees]=135854
    [fib]=2178309
    [numeric]=1999000
    [operators_loop]=149912
    [match_sum]=7995
    [option_result]=25328
    [field_hot]=4000000
    [dict_hot]=6000
    [array_mut]=2000
    [match_enum_loop]=133334146666
    [nbody]=1274223866
    [dict_count]=214964
    [for_in_sum]=12884115456
    [gc_churn]=62499500000
    [option_int_churn]=84000000
    [result_int_churn]=38000000
    [result_heap_churn]=130000000
    [host_result_unit_churn]=18000000
    [result_try_churn]=136000000
    [pair_int_churn]=200000000
)

if ! command -v poop >/dev/null 2>&1; then
    echo "poop is required for the performance matrix" >&2
    exit 1
fi
for tool in lua node; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "$tool is required for the cross-language matrix" >&2
        exit 1
    }
done

mkdir -p "$OUT_DIR"
{
    printf '# Coil performance matrix\n\n'
    printf -- '- timestamp: `%s`\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- commit: `%s`\n' "$(git rev-parse HEAD)"
    printf -- '- rustc: `%s`\n' "$(rustc --version)"
    printf -- '- duration: `%sms` per poop comparison\n\n' "$DURATION_MS"
} >"$OUT_DIR/README.md"

RUSTC_WRAPPER= cargo build --release --quiet

run_pooped() {
    local name="$1"
    shift
    local output="$OUT_DIR/${name}.poop.txt"
    {
        printf '## %s\n\n' "$name"
        poop -d "$DURATION_MS" "$@"
    } | tee "$output" >>"$OUT_DIR/README.md"
    printf '\n' >>"$OUT_DIR/README.md"
}

run_resource_sample() {
    local name="$1"
    shift
    if [[ -x /usr/bin/time ]]; then
        /usr/bin/time -f 'wall_seconds=%e\nmax_rss_kb=%M' \
            bash -c "$*" >/dev/null 2>"$OUT_DIR/${name}.resource.txt"
    else
        local started finished pid rss max_rss=0
        started="$(date +%s%N)"
        bash -c "$*" >/dev/null &
        pid=$!
        while kill -0 "$pid" 2>/dev/null; do
            if [[ -r "/proc/$pid/status" ]]; then
                rss="$(awk '/VmHWM:/ { print $2 }' "/proc/$pid/status" 2>/dev/null || true)"
                if [[ "${rss:-0}" -gt "$max_rss" ]]; then
                    max_rss="$rss"
                fi
            fi
        done
        wait "$pid"
        finished="$(date +%s%N)"
        awk -v start="$started" -v end="$finished" -v rss="$max_rss" \
            'BEGIN { printf "wall_seconds=%.6f\nmax_rss_kb=%s\n", (end-start)/1000000000, rss }' \
            >"$OUT_DIR/${name}.resource.txt"
    fi
}

check_archive() {
    local name="$1"
    local archive="$2"
    local got
    got="$("$BIN" run "$archive")"
    if [[ "$got" != "${EXPECTED[$name]}" ]]; then
        echo "checksum mismatch for $name: expected ${EXPECTED[$name]}, got $got" >&2
        return 1
    fi
    printf -- '- checksum `%s`: `%s`\n' "$name" "$got" >>"$OUT_DIR/README.md"
}

compile_perf() {
    local name="$1"
    local archive="$2"
    # Sequential baseline row: keep auto-par off so Coil matches the naive
    # Lua/Node ports rather than fork-join specializations (fib(32) especially).
    COIL_AUTO_PAR=0 "$BIN" compile "examples/perf/${name}.hy" -o "$archive" >/dev/null
}

for name in "${CROSS_LANG[@]}"; do
    # Fair sequential row vs Lua/Node (IPA off). Same sources; only COIL_AUTO_PAR differs.
    archive="$OUT_DIR/${name}.hyc"
    compile_perf "$name" "$archive"
    touch "$archive"
    check_archive "$name" "$archive"
    run_pooped "$name" \
        "$BIN run $archive" \
        "lua benchmarks/${name}.lua" \
        "node benchmarks/${name}.js"
    run_resource_sample "$name" "$BIN run $archive"

    # Default IPA on — evidence that principle-based auto-par helps when sites exist.
    archive_par="$OUT_DIR/${name}_autopar.hyc"
    "$BIN" compile "examples/perf/${name}.hy" -o "$archive_par" >/dev/null
    touch "$archive_par"
    got_par="$("$BIN" run "$archive_par")"
    if [[ "$got_par" != "${EXPECTED[$name]}" ]]; then
        echo "checksum mismatch for ${name}_autopar: expected ${EXPECTED[$name]}, got $got_par" >&2
        exit 1
    fi
    printf -- '- checksum `%s_autopar`: `%s`\n' "$name" "$got_par" >>"$OUT_DIR/README.md"
    run_pooped "${name}_autopar" \
        "$BIN run $archive_par" \
        "lua benchmarks/${name}.lua" \
        "node benchmarks/${name}.js"

    if [[ "$RUN_MASSIF" == 1 ]] && command -v valgrind >/dev/null 2>&1; then
        valgrind --tool=massif \
            --massif-out-file="$OUT_DIR/${name}.massif.out" \
            "$BIN" run "$archive" >/dev/null 2>&1 || true
    fi
done

for name in "${AOT_ONLY[@]}"; do
    archive="$OUT_DIR/${name}.hyc"
    "$BIN" compile "examples/perf/${name}.hy" -o "$archive" >/dev/null
    touch "$archive"
    check_archive "$name" "$archive"
    run_pooped "$name" "$BIN run $archive"
    run_resource_sample "$name" "$BIN run $archive"

    if [[ "$RUN_MASSIF" == 1 ]] && command -v valgrind >/dev/null 2>&1; then
        valgrind --tool=massif \
            --massif-out-file="$OUT_DIR/${name}.massif.out" \
            "$BIN" run "$archive" >/dev/null 2>&1 || true
    fi
done

echo "Performance artifacts written to $OUT_DIR"
