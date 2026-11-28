#!/bin/sh
# Build the release `libcoil_rt.a` that `coil package` links AOT executables
# with, and copy it to target/release/ (or $CARGO_TARGET_DIR/release/).
#
# On Linux std is rebuilt for it (`-Zbuild-std`, through
# RUSTC_BOOTSTRAP=1 on a stable toolchain): without std's backtrace
# symbolizer (Coil prints its own backtraces from the program's line table),
# std optimized for size, and no unwind tables (the runtime is
# `panic = "abort"`). A hello world shrinks by about 300 KB with the same
# instruction counts (docs/internals/native.md, "Size and speed").
# Elsewhere (macOS and Windows until it is measured there), or with
# COIL_RT_PLAIN=1, it is a plain `cargo build`.
#
# Needs `rustup component add rust-src` for the rebuilt std.
set -eu

target_dir="${CARGO_TARGET_DIR:-target}"
host="$(rustc -vV | sed -n 's/^host: //p')"

case "$host" in
  *-linux-gnu | *-linux-musl) small=1 ;;
  *) small=0 ;;
esac
if [ "${COIL_RT_PLAIN:-0}" = 1 ]; then
  small=0
fi

if [ "$small" = 0 ]; then
  cargo build --release -p coil-rt
  exit 0
fi

# RUSTFLAGS replaces the target's rustflags in .cargo/config.toml, so repeat
# them. The musl build's flags come in through RUSTFLAGS already.
flags="${RUSTFLAGS:-} -C force-unwind-tables=no"
case "$host" in
  x86_64-unknown-linux-gnu) flags="$flags -C link-arg=-Wl,-z,keep-text-section-prefix" ;;
esac

RUSTC_BOOTSTRAP=1 RUSTFLAGS="$flags" cargo build --release -p coil-rt \
  -Zbuild-std=std,panic_abort -Zbuild-std-features=optimize_for_size \
  --target "$host" --target-dir "$target_dir/rt-small"

mkdir -p "$target_dir/release"
cp "$target_dir/rt-small/$host/release/libcoil_rt.a" "$target_dir/release/libcoil_rt.a"
echo "built $target_dir/release/libcoil_rt.a (std rebuilt for size)"
