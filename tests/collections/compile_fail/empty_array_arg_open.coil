// @expected E0119 — nothing gives the empty `[]` argument a type (#1876).
fn keep<T>(T x) -> T {
    return x;
}

fn main() {
    let _ = keep([]);
}
