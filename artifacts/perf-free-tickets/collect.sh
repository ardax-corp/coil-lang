#!/usr/bin/env bash
# Collect investigation artifacts for docs/internals/perf-free-tickets.md
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
export COIL_AUTO_PAR=0
BIN="${BIN:-$ROOT/target/release/coil}"
R=(--root "$ROOT/.deps/coil-stdlib/src" --root "$ROOT/examples/src")
OUT="${OUT_DIR:-/workspace/artifacts/perf-free-tickets}"
mkdir -p "$OUT"/{dissect,opt-stats,times,flames,package}

BENCHES=(mandelbrot tak nsieve binary_trees fib for_in_sum vec_scan)
FLAGSHIPS=(mandelbrot tak nsieve binary_trees fib)

echo "bin=$BIN"
"$BIN" -V || true
file "$BIN" | tee "$OUT/coil.file.txt"

for name in "${BENCHES[@]}"; do
  src="examples/perf/${name}.hy"
  echo "== dissect $name =="
  "$BIN" dissect "${R[@]}" "$src" >"$OUT/dissect/${name}.txt"
  python3 "$OUT/parse_dissect.py" <"$OUT/dissect/${name}.txt" >"$OUT/dissect/${name}.hist.txt"
  echo "== compile $name =="
  "$BIN" compile "${R[@]}" --opt-stats-json "$src" -o "$OUT/${name}.hyc" \
    >"$OUT/opt-stats/${name}.stdout" 2>"$OUT/opt-stats/${name}.json"
  echo -n "run: "
  "$BIN" run "$OUT/${name}.hyc"
  echo
done

# packaged flagships
for name in "${FLAGSHIPS[@]}"; do
  echo "== package $name =="
  "$BIN" package "${R[@]}" "examples/perf/${name}.hy" -o "$OUT/package/${name}.bin"
  chmod +x "$OUT/package/${name}.bin"
  echo -n "packaged run: "
  "$OUT/package/${name}.bin"
  echo
done

TIME=/usr/bin/time
measure() {
  local label="$1"
  shift
  echo "## $label" | tee -a "$OUT/times/wall.txt"
  "$TIME" -f 'wall_seconds=%e user=%U sys=%S max_rss_kb=%M' "$@" \
    >/dev/null 2>"$OUT/times/${label}.time.txt" || true
  cat "$OUT/times/${label}.time.txt" | tee -a "$OUT/times/wall.txt"
  echo | tee -a "$OUT/times/wall.txt"
}

: >"$OUT/times/wall.txt"
for name in "${FLAGSHIPS[@]}"; do
  src="examples/perf/${name}.hy"
  measure "${name}_compile" "$BIN" compile "${R[@]}" "$src" -o "$OUT/${name}.hyc"
  measure "${name}_hyc" "$BIN" run "$OUT/${name}.hyc"
  measure "${name}_hy" "$BIN" "${R[@]}" "$src"
  measure "${name}_pkg" "$OUT/package/${name}.bin"
done

HF_RUNS="${HF_RUNS:-15}"
HF_WARM="${HF_WARM:-3}"
{
  echo "# hyperfine (COIL_AUTO_PAR=0, runs=$HF_RUNS warmup=$HF_WARM)"
  echo "bin=$BIN"
} >"$OUT/times/hyperfine.md"

for name in "${FLAGSHIPS[@]}"; do
  src="examples/perf/${name}.hy"
  echo "== hyperfine $name =="
  hyperfine --warmup "$HF_WARM" --runs "$HF_RUNS" --export-markdown "$OUT/times/${name}.hf.md" \
    --command-name "compile" "$BIN compile ${R[*]} $src -o $OUT/${name}.hyc" \
    --command-name "hyc" "$BIN run $OUT/${name}.hyc" \
    --command-name "hy" "$BIN ${R[*]} $src" \
    --command-name "pkg" "$OUT/package/${name}.bin"
  cat "$OUT/times/${name}.hf.md" >>"$OUT/times/hyperfine.md"
  echo >>"$OUT/times/hyperfine.md"
done

# hit benches (hyc only)
for name in for_in_sum vec_scan; do
  echo "== hyperfine $name hyc =="
  hyperfine --warmup "$HF_WARM" --runs "$HF_RUNS" --export-markdown "$OUT/times/${name}.hf.md" \
    --command-name "hyc" "$BIN run $OUT/${name}.hyc"
  cat "$OUT/times/${name}.hf.md" >>"$OUT/times/hyperfine.md"
done

echo "done: $OUT"
