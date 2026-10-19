// Expected: E0101 — the free `len(x)` was removed; a length is `x.len()`.
fn main() {
    let n = len([1, 2, 3]);
}
