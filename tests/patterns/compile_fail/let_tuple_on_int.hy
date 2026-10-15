// Expected: E0119 — a tuple `let` pattern cannot destructure an `int`.
fn main() {
    let (a, b) = 42;
}
