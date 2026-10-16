// `==` / `!=` on a `Result<unit, E>` with a heap or enum error type, between
// a call's result and a literal `Result` (#1193).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: true false true true true true false true

enum Fault {
    Busy,
    Gone,
}

fn f() -> Result<unit, string> {
    return Result::Ok(());
}

fn g(bool fail) -> Result<unit, string> {
    if fail {
        return Result::Err("no");
    }
    return Result::Ok(());
}

fn h(int n) -> Result<unit, Fault> {
    if n == 0 {
        return Result::Ok(());
    }
    if n == 1 {
        return Result::Err(Fault::Busy);
    }
    return Result::Err(Fault::Gone);
}

fn main() {
    let a = f() == Result::Ok(());
    let b = g(true) == Result::Ok(());
    let c = g(true) == Result::Err("no");
    let d = g(false) != Result::Err("no");
    let e = h(0) == Result::Ok(());
    let k = h(1) == Result::Err(Fault::Busy);
    let m = h(2) == Result::Err(Fault::Busy);
    let n = g(false) == g(false);
    write(stdout(), to_bytes(format("%v %v %v %v %v %v %v %v\n", a, b, c, d, e, k, m, n)));
}
