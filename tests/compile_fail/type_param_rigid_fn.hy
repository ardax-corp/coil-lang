// Expected: E0119 — a type parameter compared with an `int` is not `int` (#801).
fn generic_below<T: Ord>(Vec<T> xs, int idx) -> bool {
    return idx < xs[0];
}

fn main() {
    let v: Vec<int> = Vec::new();
    let _ = generic_below(v, 0);
}
