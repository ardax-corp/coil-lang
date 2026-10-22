// @expected E0001 — `else` must follow an `if` block.
fn main() {
    let x = 1;
    else {
        x = 2;
    }
}
