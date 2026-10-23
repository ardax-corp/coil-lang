// Sum a coil int array via C `sum_array`. Expected output: `15`.
//
// @output (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Int;
use ffi::types::Ptr;
use io::sync::print;
use string::format;

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let id = match declare(lib, "sum_array", (Ptr, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let arr = [1, 2, 3, 4, 5];
    let n = match invoke(lib, id, (arr, 5)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    let _ = print(format("%i", n));
}
