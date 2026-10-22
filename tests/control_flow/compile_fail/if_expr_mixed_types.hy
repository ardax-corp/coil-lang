// @expected E0102 — the branches of an `if` used as a value must have one type.
fn main() {
    let v = if true {
        1
    } else {
        "one"
    };
}
