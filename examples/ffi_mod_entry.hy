// ffi_mod_entry.hy — an `extern` block declared in an imported module
// (examples/src/ffi_mod/sys.hy) and called from here, with a `Vec`
// allocation between two calls.
//
// Build and grant libsum as in ffi_extern.hy:
//
//   coil --root examples/src --allow-dload sum examples/ffi_mod_entry.hy
//
// @output (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use io::sync::print;
use string::format;
use ffi_mod::sys::run_twice;

fn main() {
    let _ = print(format("%i", run_twice()));
}
