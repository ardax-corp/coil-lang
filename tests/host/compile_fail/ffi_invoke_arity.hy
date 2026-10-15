// Expected: E0405 — `invoke` takes (lib, fn_id, args_tuple).
use ffi::{dload, invoke};

fn main() {
    let lib = match dload("x.so") {
        Result::Ok(h) => h,
        Result::Err(_) => 0,
    };
    let id = 0;
    invoke(lib, id);
}
