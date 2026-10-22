// @expected E0102 — a string's byte index must be an `int` (#1172).
fn main() {
    let s = "abc";
    let c = s["a"];
}
