// @expected E0100 — a lambda sees an outer local only through `use (k)`.
fn main() {
    let k = 1;
    let g = fn (int x) => x + k;
    let r = g(1);
}
