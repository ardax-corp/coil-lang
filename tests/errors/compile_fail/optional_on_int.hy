// Expected: E0116 — `?.` needs an Option on the left.
fn main() {
    let n = 3;
    let r = n?.v;
}
