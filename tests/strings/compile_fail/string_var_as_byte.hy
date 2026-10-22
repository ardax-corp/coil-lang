// @expected E0102 — only literals coerce to `byte`; a `string` variable does not.
fn main() {
    let s = "a";
    let b: byte = s;
}
