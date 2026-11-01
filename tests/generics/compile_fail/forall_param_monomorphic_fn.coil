// @expected E0102 — a `forall T. T -> T` parameter needs a polymorphic function;
// the named `int -> int` function `inc` is not one.
fn inc(int x) -> int {
    return x;
}

fn app(forall T. T -> T f, int x) -> int {
    return f(x);
}

fn main() {
    let r = app(inc, 1);
}
