// Expected: E0102 — `s <<= 1` checks like `s = s << 1`, and `<<` is not
// defined on a string (#883).
fn main() {
    let s = "ab";
    s <<= 1;
}
