// @expected E0119 — `bool` has no `Ord` instance.
fn max_of<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}

fn main() {
    let b = max_of(true, false);
}
