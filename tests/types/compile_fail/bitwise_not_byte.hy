// @expected E0102 — `~` is defined on `int` only, not on `byte`.
fn main() {
    let b: byte = 1;
    let c = ~b;
}
