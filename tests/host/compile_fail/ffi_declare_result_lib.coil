// @expected E0102 — `declare`'s library handle must be the `int` that `dload`
// returns, not its unmatched `Result`; unwrap it with `?` or `match` (#1787).
use ffi::declare;
use ffi::Error;
use ffi::types::Int;

fn bind(Result<int, Error> lib) {
    let _ = declare(lib, "sum", (Int, Int), Int);
}

fn main() {
    bind(Result::Ok(7));
}
