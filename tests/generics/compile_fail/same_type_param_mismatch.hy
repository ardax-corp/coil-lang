// @expected E0102 — both arguments are `T`, so they must have one type.
fn pick<T>(T a, T b) -> T {
    return a;
}

fn main() {
    let x = pick(1, "a");
}
