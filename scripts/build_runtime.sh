#!/usr/bin/env bash
# Build the release runtime library AOT executables link (`coil-rt`,
# `libcoil_rt.a`) and put it at target/release/libcoil_rt.a.
#
# On Linux it rebuilds std for it (`-Zbuild-std`, through RUSTC_BOOTSTRAP on
# stable): std without its backtrace symbolizer (gimli, addr2line, demangle;
# Coil backtraces come from the program's line table), std optimized for
# size, and no unwind tables (`panic = "abort"`). A hello world is ~300 KB
# smaller and runs the same instructions (docs/internals/native.md, "Size
# and speed"). Elsewhere, or with COIL_RT_PLAIN=1, it is a plain
# `cargo build --release -p coil-rt`.
#
#   scripts/build_runtime.sh
set -euo pipefail

host="$(rustc -vV | sed -n 's/^host: //p')"
case "$host" in
    *-linux-gnu) ;;
    *) COIL_RT_PLAIN=1 ;;
esac
if [[ "${COIL_RT_PLAIN:-}" == 1 ]]; then
    cargo build --release -p coil-rt
    exit 0
fi

if command -v rustup >/dev/null; then
    rustup component add rust-src >/dev/null
fi
target_dir="${CARGO_TARGET_DIR:-target}"
# RUSTFLAGS replaces .cargo/config.toml's target rustflags: those are link
# arguments, which a static library does not take.
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C force-unwind-tables=no" \
    cargo build --release -p coil-rt \
    -Zbuild-std=std,panic_abort -Zbuild-std-features=optimize_for_size \
    --target "$host" --target-dir "$target_dir/rt-std"
mkdir -p "$target_dir/release"
rm -f "$target_dir/release/libcoil_rt.a"
cp "$target_dir/rt-std/$host/release/libcoil_rt.a" "$target_dir/release/libcoil_rt.a"
