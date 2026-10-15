// Expected: E0119 — unary `-` on an array needs numeric elements.
fn main() {
    let c = -[true, false];
}
