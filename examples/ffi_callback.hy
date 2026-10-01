// C calls back into coil via ffi::types::Callback. Expected output: `42`.
//
// Output: (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Callback;
use ffi::types::Int;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn doubler(int x) -> int {
    return x * 2;
}

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let id = match declare(lib, "apply_cb", (Callback, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let n = match invoke(lib, id, (doubler, 21)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    write_all(stdout(), to_bytes(format("%i", n)));
}
