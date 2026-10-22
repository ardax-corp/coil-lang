// @expected E0100 — a `use` capture must name a value in scope.
fn main() {
    let g = fn (int x) use (missing) => x;
    let r = g(1);
}
