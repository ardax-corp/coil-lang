// @expected E0102 — an under-applied call is a partial function, not its result.
fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let r: int = add(1);
}
