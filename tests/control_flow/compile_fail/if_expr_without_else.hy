// @expected E0137 — an `if` used as a value needs an `else`.
fn main() {
    let v = if true {
        1
    };
}
