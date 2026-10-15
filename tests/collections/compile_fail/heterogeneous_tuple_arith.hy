// Expected: E0119 — element-wise arithmetic needs a homogeneous numeric tuple.
fn main() {
    let c = (1, 2.0) + (1, 2.0);
}
