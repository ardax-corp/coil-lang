// ffi_extern.hy — compile-time FFI: an `extern "lib" { … }` block binds C
// functions, and calls to them look like ordinary coil calls.
//
// Uses `sum` from examples/sum.c. Build the library first (from the repo
// root), then grant it:
//
//   Linux:  cc -shared -fPIC -o examples/libsum.so examples/sum.c
//   macOS:  cc -dynamiclib -o examples/libsum.dylib examples/sum.c
//
//   coil --allow-dload sum examples/ffi_extern.hy
//
// `coil.toml`'s `[ffi] search_paths` finds `libsum` in ./examples. Every
// library stem needs `--allow-dload STEM`; the libc aliases (`extern "c"`)
// are always denied.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

extern "sum" {
    fn sum(int a, int b) -> int;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", sum(40, 2))));
}
