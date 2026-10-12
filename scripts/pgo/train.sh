#!/usr/bin/env bash
# PGO / BOLT training workload for `coil` and `coil-embed`.
#
# Deliberately broad and benchmark-free: examples/perf and the perf-checkup
# benches are the hold-out set used to judge the optimized build, so they never
# feed the profile. What runs here:
#   1. the compiler on every example, language test and a large real program
#      (spool), so compile-time code gets a profile too;
#   2. every example program run to completion (`coil x.hy`), as a .hyc
#      (`coil run`) and as a packaged executable (`coil-embed`);
#   3. the language test corpus and the pure-Coil library test suites through
#      `coil test` (real parsing, formatting, collections and string work).
#
# Usage: scripts/pgo/train.sh BIN_DIR [SIBLINGS_DIR]
#   BIN_DIR       holds the instrumented coil, coil-embed and coil-test
#   SIBLINGS_DIR  parent of the coil-stdlib / coil-json / ... checkouts
#                 (default: the directory above this repo)
# Failures of individual programs are counted, not fatal: an example that
# needs stdin or a native library still contributes the code it reached.

set -u
BIN=$(cd "$1" && pwd)
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SIB=$(cd "${2:-$REPO/..}" && pwd)
COIL="$BIN/coil"
EMBED="$BIN/coil-embed"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
TIMEOUT=${PGO_TRAIN_TIMEOUT:-20}

ok=0
fail=0
step() {
    if timeout "$TIMEOUT" "$@" </dev/null >/dev/null 2>&1; then
        ok=$((ok + 1))
    else
        fail=$((fail + 1))
        [ -n "${PGO_TRAIN_VERBOSE:-}" ] && echo "failed: $*" >&2
    fi
}

STDLIB="$SIB/coil-stdlib/src"
# -A: examples and suites read files, spawn processes and use the network.
ROOTS=(-A --root "$STDLIB" --root "$REPO/examples/src")
[ -d "$SIB/coil-time/src" ] && ROOTS+=(--root "$SIB/coil-time/src")

# 1 + 2: examples — compile, run from source, run the .hyc, run packaged.
for f in "$REPO"/examples/*.hy; do
    name=$(basename "$f" .hy)
    step "$COIL" "${ROOTS[@]}" "$f"
    step "$COIL" compile "${ROOTS[@]}" "$f" -o "$WORK/$name.hyc"
    [ -f "$WORK/$name.hyc" ] && step "$COIL" run -A "$WORK/$name.hyc"
    if [ -x "$EMBED" ]; then
        step "$COIL" package "${ROOTS[@]}" --runner "$EMBED" "$f" -o "$WORK/$name.app"
        [ -x "$WORK/$name.app" ] && step "$WORK/$name.app"
    fi
done

# 1: compiler only — language tests (with their test cases) and spool.
for f in "$REPO"/tests/positive/*.hy; do
    step "$COIL" compile --include-tests "${ROOTS[@]}" "$f" -o "$WORK/t.hyc"
done
if [ -f "$SIB/spool/spool.hy" ]; then
    SPOOL_ROOTS=(--root "$SIB/spool/src")
    for dep in coil-toml coil-json; do
        [ -d "$SIB/$dep/src" ] && SPOOL_ROOTS+=(--root "$SIB/$dep/src")
    done
    for _ in 1 2 3; do
        step "$COIL" compile "${ROOTS[@]}" "${SPOOL_ROOTS[@]}" "$SIB/spool/spool.hy" -o "$WORK/spool.hyc"
    done
fi

# 3: test suites through `coil test` (runs coil-test beside coil).
(cd "$REPO" && TIMEOUT=900 step "$COIL" test "${ROOTS[@]}" tests/positive)
for lib in coil-stdlib coil-json coil-toml coil-csv coil-url coil-uuid coil-msgpack \
    coil-deflate coil-gzip coil-time coil-log; do
    [ -d "$SIB/$lib/tests" ] || continue
    lib_roots=("${ROOTS[@]}")
    for dep in coil-deflate coil-json; do
        [ "$dep" != "$lib" ] && [ -d "$SIB/$dep/src" ] && lib_roots+=(--root "$SIB/$dep/src")
    done
    (cd "$SIB/$lib" && TIMEOUT=900 step "$COIL" test "${lib_roots[@]}")
done

echo "pgo training: $ok steps ok, $fail failed"
