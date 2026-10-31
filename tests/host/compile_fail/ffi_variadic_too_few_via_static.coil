// @expected E0405 — the variadic `declare` stored in the static `fn_id` has
// two fixed parameters; `invoke` passes one.
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Int;
use ffi::types::Float;

static let fn_id = 0;

fn call(int lib) -> Result<float, Error> {
    let f: float = invoke(lib, fn_id, (1,))?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = 7;
    fn_id = declare(lib, "f", (Int, Int), Float, true)?;
    let _ = call(lib)?;
}
