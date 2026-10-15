// Expected: E0400 — a literal out-of-range index is rejected for stores too.
fn main() {
    let a = [1, 2];
    a[2] = 5;
}
