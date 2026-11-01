// @expected E0119 — `T: Add` allows `+` only; `*` needs a `Mul` bound.
fn bad<T: Add>(T a, T b) -> T {
    return a * b;
}

fn main() {}
