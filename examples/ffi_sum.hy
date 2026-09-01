// ffi_sum.hy — Userland FFI example. Demonstrates the
// `dload` / `declare` / `invoke` API surface that lets a
// script load a C library, declare its signatures, and call
// functions entirely from source — no VM-recompile-time
// definitions required.
//
// The corresponding C source is `sum.c` in this directory:
//
//   int64_t sum(int64_t a, int64_t b) { return a + b; }
//
// Build the shared library (from repo root):
//
//   Linux:  cc -shared -fPIC -o examples/libsum.so examples/sum.c
//   macOS:  cc -dynamiclib -o examples/libsum.dylib examples/sum.c
//   Windows: clang -shared -o examples/sum.dll examples/sum.c
//
// `dload("sum")` resolves to the platform filename via
// `[ffi] search_paths` in `coil.toml` (./examples). Every stem needs
// `--allow-dload STEM` plus a matching `[[package.native]] sha256` or
// `trusted = true` on that dep — including `time` / `crypto` / `tls` /
// `regex`. A stem without allow is `LibraryDenied`. `dload("c")` is
// always denied; an absolute path is not a bypass.
//
// Each of dload / declare / invoke returns `Result<_, Error>`;
// unwrap with match (or `?` in a Result-returning function).
//
// Expected output: `42` (40 + 2).

use ffi::{declare, dload, invoke};
use ffi::types::{Int};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let sum_id = match declare(lib, "sum", (Int, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let n = match invoke(lib, sum_id, (40, 2)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    write_all(stdout(), to_bytes(format("%i", n)));
}
