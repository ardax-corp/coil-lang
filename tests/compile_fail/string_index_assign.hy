// Expected: E0107 — strings are immutable; `s[i]` only reads.
fn main() {
    let s = "abc";
    s[0] = "x";
}
