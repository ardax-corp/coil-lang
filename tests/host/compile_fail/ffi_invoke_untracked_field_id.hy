// Expected: E0102 — `api.id` never holds a `declare` result, so `invoke` on it
// is not refined to `float`.
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Float;

class Api {
    pub id: int,
}

fn main() -> Result<(), Error> {
    let lib = 7;
    let api = new Api(0);
    let _ = declare(lib, "f", (), Float)?;
    let f: float = invoke(lib, api.id, ())?;
    let _ = f;
}
