// ffi_varargs.hy — C-style varargs: a bare trailing `...` on an extern
// declaration (not a coil rest parameter `T... xs`). The call is prepared
// per call, and the variadic arguments get C's default promotions.
//
// Uses `sum_n` from examples/sum.c (`int64_t sum_n(int64_t n, ...)`); build
// and grant the library as in ffi_extern.hy:
//
//   coil --allow-dload sum examples/ffi_varargs.hy
//
// coil `int` is a 64-bit integer, which matches the C side's `int64_t`
// `va_arg`.
//
// Output: 60

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

extern "sum" {
    fn sum_n(int n, ...) -> int;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", sum_n(3, 10, 20, 30))));
}
