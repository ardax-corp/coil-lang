// Expected: E0119 — `/` is not a `Matrix` operator.
fn main() {
    let a = matrix([[1, 2], [3, 4]]);
    let _ = a / a;
}
