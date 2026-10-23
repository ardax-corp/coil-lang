// @expected E0001 — `_` goes between digits only (#1849).
fn main() {
    let _ = 0x_ff;
}
