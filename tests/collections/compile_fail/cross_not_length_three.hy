// Expected: E0119 — `cross` is defined on length-3 vectors only.
fn main() {
    let _ = cross((1, 2), (3, 4));
}
