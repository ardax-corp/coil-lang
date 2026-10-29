// @expected E0102 — an imported function keeps its parameter types.
use calc::add;

fn main() {
    let _ = add("a", 2);
}
