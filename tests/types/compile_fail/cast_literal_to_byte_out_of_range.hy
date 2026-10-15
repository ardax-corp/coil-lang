// Expected: E0102 — a literal `int as byte` outside 0..=255 is rejected at
// compile time (only non-literal values wrap).
fn main() {
    let b = 300 as byte;
}
