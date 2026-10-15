// Expected: E0107 — `readonly [..]` cannot be written through an index.
fn main() {
    let xs = readonly [1, 2, 3];
    xs[0] = 9;
}
