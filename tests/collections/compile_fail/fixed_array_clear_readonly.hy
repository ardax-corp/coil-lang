// @expected E0412 — a `readonly` fixed array is still `[T; N]`, so `clear` is rejected.
fn main() {
    let xs = readonly [1, 2];
    xs.clear();
}
