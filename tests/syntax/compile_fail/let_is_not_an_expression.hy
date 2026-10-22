// @expected E0001 — `let` is a statement; it cannot be the initializer of
// another `let`.
fn main() {
    let y = let x = 5;
}
