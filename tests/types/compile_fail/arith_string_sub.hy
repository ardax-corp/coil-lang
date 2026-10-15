// Expected: E0102 — `-` on strings (only `+` concatenates); the
// VM would subtract the two pointers (#554).
fn main() {
    let a = "a" - "b";
}
