// Expected: E0102 — `invoke`'s function id must be the `int` handle, not the
// `Result` that `declare` returns; unwrap it with `?` or `match` (#1761).
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Float;

fn main() {
    let lib = 7;
    let id = declare(lib, "f", (), Float);
    let r: Result<float, Error> = invoke(lib, id, ());
}
