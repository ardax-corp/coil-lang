// Expected: E0405 — the variadic `declare` stored in `api.id` has two fixed
// parameters; `invoke` passes one.
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Int;
use ffi::types::Float;

class Api {
    pub id: int,
}

fn main() -> Result<(), Error> {
    let lib = 7;
    let api = new Api(0);
    api.id = declare(lib, "f", (Int, Int), Float, true)?;
    let f: float = invoke(lib, api.id, (1,))?;
    let _ = f;
}
