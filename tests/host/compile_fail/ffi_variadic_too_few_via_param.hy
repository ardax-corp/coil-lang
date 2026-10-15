// Expected: E0405 — the variadic `declare` has two fixed parameters, and the
// id reaches `invoke` through `helper`'s parameter with only one argument.
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Int;
use ffi::types::Float;

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, (1,))?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = 7;
    let fn_id = declare(lib, "f", (Int, Int), Float, true)?;
    let _ = helper(fn_id)?;
}
