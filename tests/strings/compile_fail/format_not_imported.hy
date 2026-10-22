// @expected E0101 — `format` lives in the `string` module and needs `use string::format`.
fn main() {
    let s = format("%i", 1);
}
