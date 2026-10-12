#!/usr/bin/env bash
# PGO + BOLT release build of `coil` and `coil-embed` (Linux x86_64).
#
#   1. PGO: instrumented build, run scripts/pgo/train.sh, rebuild with the
#      profile (keeping relocations so BOLT can rewrite the result).
#   2. BOLT: instrument that build, run the same workload again, then let BOLT
#      reorder and align the hot code (the VM dispatch loop above all).
#
# Why: the VM's speed depends on where its dispatch loop lands in the binary
# (docs/internals/release-pgo.md). A profile places it on purpose in every
# release instead of leaving it to the linker.
#
# Needs: cargo-pgo (`cargo install cargo-pgo`), the `llvm-tools` rustup
# component, llvm-bolt and merge-fdata on PATH (LLVM 18+), and the sibling
# checkouts train.sh uses (coil-stdlib and the pure-Coil libraries).
# Output: target/pgo-bolt/{coil,coil-embed}
#
# Usage: scripts/pgo/build.sh [SIBLINGS_DIR]

set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SIB=$(cd "${1:-$REPO/..}" && pwd)
cd "$REPO"

OUT=target/x86_64-unknown-linux-gnu/release
WORK=target/pgo-work
DEST=target/pgo-bolt
export RUSTFLAGS="-C link-arg=-Wl,-z,keep-text-section-prefix"
BINS=(-p coil -p coil-embed -p coil-test)

rm -rf target/pgo-profiles "$WORK" "$DEST"
mkdir -p "$WORK/train" "$DEST"

# --- PGO ---------------------------------------------------------------------
cargo pgo build -- "${BINS[@]}"
for b in coil coil-embed coil-test; do cp "$OUT/$b" "$WORK/train/$b"; done
LLVM_PROFILE_FILE="$REPO/target/pgo-profiles/coil_%m_%p.profraw" \
    scripts/pgo/train.sh "$WORK/train" "$SIB"

# BOLT needs relocations (-q) and the symbol table (no strip).
RUSTFLAGS="$RUSTFLAGS -C link-arg=-Wl,-q" CARGO_PROFILE_RELEASE_STRIP=false \
    cargo pgo optimize build -- "${BINS[@]}"

# --- BOLT --------------------------------------------------------------------
# BOLT's instrumentation runtime aborts on symbol names longer than its
# buffer, and drop glue for the parser's combinator types has names of
# hundreds of KB. Rename long local symbols to short unique ones first.
for b in coil coil-embed; do
    nm "$OUT/$b" | awk 'length($3) > 200 { print $3 }' | sort -u \
        | awk '{ printf "%s __coil_sym_%d\n", $0, NR }' > "$WORK/$b.syms"
    objcopy --redefine-syms="$WORK/$b.syms" "$OUT/$b" "$WORK/$b.pgo"
    mkdir -p "$WORK/fdata/$b"
    llvm-bolt -instrument "$WORK/$b.pgo" -o "$WORK/train/$b" \
        --instrumentation-file="$REPO/$WORK/fdata/$b/prof" \
        --instrumentation-file-append-pid
done
cp "$OUT/coil-test" "$WORK/train/coil-test"
PGO_TRAIN_TIMEOUT=120 scripts/pgo/train.sh "$WORK/train" "$SIB"

for b in coil coil-embed; do
    merge-fdata "$WORK"/fdata/$b/*.fdata > "$WORK/$b.fdata"
    llvm-bolt "$WORK/$b.pgo" -o "$DEST/$b" -data "$WORK/$b.fdata" \
        -reorder-blocks=ext-tsp -reorder-functions=hfsort -split-functions \
        -split-all-cold -jump-tables=move -use-gnu-stack -split-eh \
        -icf=1 -use-old-text -dyno-stats
    strip "$DEST/$b"
done
[ -n "${PGO_KEEP_WORK:-}" ] || rm -rf "$WORK"
ls -l "$DEST"
