// @expected E0404 — `declare` takes (lib, name, args_tuple, ret_type[, variadic]).
use ffi::dload;
use ffi::declare;
use ffi::types::Int;

fn main() {
    let lib = match dload("x.so") {
        Result::Ok(h) => h,
        Result::Err(_) => 0,
    };
    declare(lib, "f", Int);
}
