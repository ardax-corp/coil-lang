// ffi_mod_entry.hy — an `extern` block declared in an imported module
// (examples/src/ffi_mod/sys.hy) and called from here, with a `Vec`
// allocation between two calls.
//
// Build and grant libsum as in ffi_extern.hy:
//
//   coil --root examples/src --allow-dload sum examples/ffi_mod_entry.hy
//
// Output: (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use ffi_mod::sys::run_twice;

fn main() {
    write_all(stdout(), to_bytes(format("%i", run_twice())));
}
