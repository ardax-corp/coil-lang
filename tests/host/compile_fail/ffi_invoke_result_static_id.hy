// Expected: E0102 — a `static` holding `declare`'s unmatched `Result` is not
// an `int` function id; unwrap it with `?` or `match` (#1731, #1761).
use ffi::declare;
use ffi::invoke;
use ffi::Error;
use ffi::types::Float;

static let f = declare(7, "f", (), Float);

fn main() {
    let r: Result<float, Error> = invoke(7, f, ());
}
