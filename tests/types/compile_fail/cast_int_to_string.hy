// @expected E0102 — `as` converts between primitive scalars; `int` does
// not cast to `string` (use `format`).
fn main() {
    let s = 5 as string;
}
