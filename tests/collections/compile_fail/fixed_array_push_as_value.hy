// @expected E0412 — naming `xs.push` as a value on a `[T; N]` is rejected too.
fn main() {
    let xs = [1, 2, 3];
    let _ = xs.push;
}
