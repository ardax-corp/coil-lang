// @expected E0102 — an `int -> int` value does not fit a `fn() -> int` parameter.
fn at(fn() -> int f) -> int {
    return f();
}

fn inc(int x) -> int {
    return x + 1;
}

fn main() {
    let r = at(inc);
}
