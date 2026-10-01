// examples/ffi_callback_ret.hy — FFI callback/function-pointer return.
//
// get_doubler() returns a C function pointer as an opaque Ptr value.
// address as a coil callback requires a separate declare/host
// trampoline (not automatic in this phase).
//
// Output: (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Ptr;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let id = match declare(lib, "get_doubler", (), Ptr) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let ptr = match invoke(lib, id, ()) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    // Non-zero pointer address — print low 16 bits as a smoke check
    // that we got a real address back (not -1 / null).
    if ptr == 0 {
        write_all(stdout(), to_bytes(format("%i", 0)));
    } else {
        write_all(stdout(), to_bytes(format("%i", 1)));
    }
}
