// @expected E0102 — `a |= n` on a byte checks like `a = a | n`: both
// operands of `|` must be bytes (#883); only a literal coerces (#2093).
fn main() {
    let a = 7 as byte;
    let n = 8;
    a |= n;
}
