// @expected E0102 — every value packed into `int... xs` must be an `int`.
fn count(int... xs) -> int {
    return xs.len();
}

fn main() {
    let r = count(1, "two", 3);
}
