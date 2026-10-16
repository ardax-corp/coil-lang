// Expected: E0102 — an array index must be an `int`, not a `float` (#1172).
fn main() {
    let a = [1, 2];
    let x = a[1.0];
}
