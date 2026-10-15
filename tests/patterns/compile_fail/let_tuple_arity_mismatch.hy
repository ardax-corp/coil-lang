// Expected: E0119 — a tuple `let` pattern must have as many items as the value.
fn main() {
    let (a, b) = (1, 2, 3);
}
