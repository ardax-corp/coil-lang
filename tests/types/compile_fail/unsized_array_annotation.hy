// Expected: E0119 — `[T]` without a length is not a type; write `[T; N]`
// or `Vec<T>`.
fn main() {
    let xs: [int] = [1, 2, 3];
}
