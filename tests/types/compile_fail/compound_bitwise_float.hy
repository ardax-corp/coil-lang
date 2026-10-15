// Expected: E0102 — `f &= 2` checks like `f = f & 2`, and `&` is not
// defined on a float (#883).
fn main() {
    let f = 1.5;
    f &= 2;
}
