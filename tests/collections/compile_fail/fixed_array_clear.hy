// @expected E0412 — `clear` changes the length; `[T; N]` cannot shrink.
fn main() {
    let a = [1, 2];
    a.clear();
}
