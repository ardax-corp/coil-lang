// Expected: E0102 — `a |= 8` on a byte checks like `a = a | 8`: both
// operands of `|` must be bytes (#883).
fn main() {
    let a = 7 as byte;
    a |= 8;
}
