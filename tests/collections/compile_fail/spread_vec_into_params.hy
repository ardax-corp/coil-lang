// @expected E0119 — a `Vec` has no static length, so `f(...v)` cannot spread it into
// fixed parameters.
fn rows() -> Vec<int> {
    return [1, 2, 3];
}

fn f(int x) -> int {
    return x;
}

fn main() {
    f(...rows());
}
