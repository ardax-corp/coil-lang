// Expected: E0102 — `s += x` needs `x` to be a string.
fn main() {
    let s = "a";
    s += 1;
}
