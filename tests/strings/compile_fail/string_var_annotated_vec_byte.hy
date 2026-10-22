// @expected E0102 — only a string literal coerces to `Vec<byte>` through an
// annotation; a `string` variable does not.
fn main() {
    let s = "hi";
    let _: Vec<byte> = s;
}
