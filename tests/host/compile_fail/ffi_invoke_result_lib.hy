// Expected: E0102 — `invoke`'s library handle must be the `int` that `dload`
// returns, not its unmatched `Result`; unwrap it with `?` or `match` (#1787).
use ffi::{invoke, Error};

fn call(Result<int, Error> lib, int id) {
    let r: Result<int, Error> = invoke(lib, id, (1, 2));
}

fn main() {
    call(Result::Ok(7), 0);
}
