// examples/ffi_struct_ret.hy — FFI struct return via make_point.
//
// @output (not checked: needs `--allow-dload sum`; see the example_ffi_* pipeline tests)

use ffi::Error;
use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Int32;
use io::sync::print;
use string::format;

extern struct Point {
    x: int32,
    y: int32,
};

fn main() -> Result<(), Error> {
    let lib = dload("sum")?;
    let make_id = declare(lib, "make_point", (Int32, Int32), Point)?;
    let p = invoke(lib, make_id, (3, 4))?;
    let _ = print(format("%i", p.x));
    let _ = print(format("%i", p.y));
}
