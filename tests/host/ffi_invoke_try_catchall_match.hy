// A match with a lone binding arm on `invoke(...)?` lowers whatever the
// declared return type (#1658). The handle `7` is not a loaded library, so
// `declare` fails with `InvalidHandle` and `?` returns it.
use tests::assert;
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Int;
use ffi::types::Float;
use ffi::types::String;

fn call_float(int lib) -> Result<float, Error> {
    let f = declare(lib, "nat_scale", (Int, Int), Float)?;
    let r = match invoke(lib, f, (1, 2))? {
        x => x + 1.0,
    };
    return Result::Ok(r);
}

fn call_string(int lib) -> Result<int, Error> {
    let f = declare(lib, "nat_name", (Int, Int), String)?;
    return match invoke(lib, f, (1, 2))? {
        s => Result::Ok(s.len()),
    };
}

test("a binding arm on invoke(...)? of a float and a string") {
    assert!(
        match call_float(7) {
            Result::Ok(_) => false,
            Result::Err(_) => true,
        },
    );
    assert!(
        match call_string(7) {
            Result::Ok(_) => false,
            Result::Err(_) => true,
        },
    );
}
