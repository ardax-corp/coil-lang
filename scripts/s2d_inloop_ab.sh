#!/usr/bin/env bash
# S2e A/B: parent vs tip fuse (default) vs tip dense (`COIL_S2D_DENSE_INLOOP=1`).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/coil-s2d-inloop-ab}"
PARENT_BIN="${PARENT_BIN:-/tmp/coil-s3b/target/release/coil}"
CUR_BIN="${CUR_BIN:-$ROOT/target/release/coil}"
DISSECT_BIN="${DISSECT_BIN:-$ROOT/target/release/coil-dissect}"
STDLIB_SRC="${STDLIB_SRC:-$ROOT/.deps/coil-stdlib/src}"
ROOTS=()
if [[ -d "$STDLIB_SRC" ]]; then
  ROOTS=(--root "$STDLIB_SRC")
fi
mkdir -p "$OUT"

benches=(
  examples/perf/s2d_inloop_pack.hy
  examples/perf/s2d_inloop_pack_store.hy
  examples/perf/s2d_inloop_pack_arith.hy
  examples/perf/s2d_inloop_pack_wide.hy
  examples/perf/s2d_preheader_bump.hy
  examples/perf/looping_makearray.hy
)

fn_for() {
  case "$1" in
    *preheader_bump*|*looping_makearray*) echo bump ;;
    *) echo pack ;;
  esac
}

count_ops() {
  local dump="$1"
  python3 - "$dump" <<'PY'
import collections, re, sys
text = open(sys.argv[1]).read()
ops = re.findall(r'\b([A-Za-z][A-Za-z0-9]*)\b', text)
# crude: keep likely mnemonics from dissect lines that look like bytecode
keys = ("Seek","MakeArray","DenseBin","DenseCmp","DenseConst","DenseMove",
        "LOAD","STORE","StorePop","Index","IndexUnchecked","StoreIndex",
        "StoreIndexUnchecked","CALL","RETURN","JMP","JMPF","JMPT",
        "ADD","MOD","INC","ArrayLen")
c = collections.Counter()
for line in text.splitlines():
    for k in keys:
        if k in line:
            c[k] += line.count(k)
print(" ".join(f"{k}={c[k]}" for k in keys if c[k]))
PY
}

echo "== binaries =="
ls -l "$PARENT_BIN" "$CUR_BIN" || true
"$CUR_BIN" --version || true

for path in "${benches[@]}"; do
  name="$(basename "$path" .hy)"
  fn="$(fn_for "$path")"
  echo
  echo "======== $name ($fn) ========"
  "$PARENT_BIN" compile "${ROOTS[@]}" "$ROOT/$path" -o "$OUT/${name}.parent.hyc"
  "$CUR_BIN" compile "${ROOTS[@]}" "$ROOT/$path" -o "$OUT/${name}.tip.hyc"
  COIL_S2D_DENSE_INLOOP=1 "$CUR_BIN" compile "${ROOTS[@]}" "$ROOT/$path" -o "$OUT/${name}.dense.hyc"
  sha256sum "$OUT/${name}.parent.hyc" "$OUT/${name}.tip.hyc" "$OUT/${name}.dense.hyc"
  echo "-- checksums --"
  "$CUR_BIN" run "$OUT/${name}.parent.hyc" | tee "$OUT/${name}.parent.out"
  "$CUR_BIN" run "$OUT/${name}.tip.hyc" | tee "$OUT/${name}.tip.out"
  "$CUR_BIN" run "$OUT/${name}.dense.hyc" | tee "$OUT/${name}.dense.out"
  if [[ -x "$DISSECT_BIN" ]]; then
    PARENT_DISSECT="${PARENT_DISSECT:-$(dirname "$PARENT_BIN")/coil-dissect}"
    if [[ -x "$PARENT_DISSECT" ]]; then
      "$PARENT_DISSECT" "${ROOTS[@]}" "$ROOT/$path" --fn "$fn" > "$OUT/${name}.parent.dissect.txt" || true
    fi
    "$DISSECT_BIN" "${ROOTS[@]}" "$ROOT/$path" --fn "$fn" > "$OUT/${name}.tip.dissect.txt" || true
    COIL_S2D_DENSE_INLOOP=1 "$DISSECT_BIN" "${ROOTS[@]}" "$ROOT/$path" --fn "$fn" > "$OUT/${name}.dense.dissect.txt" || true
    echo "-- opcode mix tip --"
    count_ops "$OUT/${name}.tip.dissect.txt"
    echo "-- opcode mix dense --"
    count_ops "$OUT/${name}.dense.dissect.txt"
  fi
  hyperfine -w 2 -r 8 --export-markdown "$OUT/${name}.md" \
    "$CUR_BIN run $OUT/${name}.parent.hyc" \
    "$CUR_BIN run $OUT/${name}.tip.hyc" \
    "$CUR_BIN run $OUT/${name}.dense.hyc"
done
echo "done: $OUT"
