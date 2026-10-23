// @expected E0119 — an `int` parameter cannot take an empty `[]` (#1876).
fn twice(int x) -> int {
    return x * 2;
}

fn main() {
    let _ = twice([]);
}
