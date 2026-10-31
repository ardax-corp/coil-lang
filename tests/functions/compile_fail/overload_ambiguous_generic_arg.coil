// @expected E0122 — with `x: T` open, both `f(int)` and `f(float)` fit the call.
fn f(int x) -> int {
    return x;
}

fn f(float x) -> float {
    return x;
}

fn g<T>(T x) -> int {
    return f(x);
}

fn main() {}
