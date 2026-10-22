// @expected E0102 — a `forall T. T -> T` parameter needs a polymorphic function; an
// `int -> int` lambda is not one.
fn app(forall T. T -> T f, int x) -> int {
    return f(x);
}

fn main() {
    let n = app(fn (int v) => v + 1, 1);
}
