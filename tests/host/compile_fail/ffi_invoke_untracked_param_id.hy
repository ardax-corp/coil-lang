// Expected: E0102 — `helper` is only called with `0`, not a `declare`d id, so
// its `invoke` result is not refined to `float`.
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Float;

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = 7;
    let _ = declare(lib, "f", (), Float)?;
    let _ = helper(0)?;
}
