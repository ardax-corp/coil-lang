// Expected: E0119 — two `Vec`s do not zip (lengths are unknown); use `[T; N]` or a scalar.
fn main() {
    let a = Vec::from([1, 2]);
    let b = Vec::from([3, 4]);
    let c = a + b;
}
