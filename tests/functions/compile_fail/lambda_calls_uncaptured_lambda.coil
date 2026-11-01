// @expected E0101 — a lambda calling an outer lambda-valued local must list
// it in `use (g)`.
fn main() {
    let g = fn (int x) => x;
    let f = fn (int x) => g(x);
    f(1);
}
