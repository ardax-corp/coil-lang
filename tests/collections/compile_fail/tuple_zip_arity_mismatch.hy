// Expected: E0119 — element-wise tuple operators need equal arities.
fn main() {
    let c = (1, 2) + (1, 2, 3);
}
