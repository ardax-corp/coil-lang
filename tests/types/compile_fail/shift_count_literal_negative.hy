// @expected E0131 — a negative literal shift count is rejected (#1076).
fn main() {
    let n = 5;
    n >>= -1;
}
