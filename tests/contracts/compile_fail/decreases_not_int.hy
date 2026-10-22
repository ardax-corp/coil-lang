// @expected E0102 — a `decreases` clause must be an `int`.
fn main() {
    let i = 0;
    while i < 3
        decreases i < 3
    {
        i = i + 1;
    }
}
