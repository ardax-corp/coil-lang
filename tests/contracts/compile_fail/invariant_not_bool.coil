// @expected E0102 — a loop `invariant` must be a `bool`.
fn main() {
    let i = 0;
    while i < 3
        invariant i
    {
        i = i + 1;
    }
}
