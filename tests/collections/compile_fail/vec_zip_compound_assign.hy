// Expected: E0102 — `v += w` with two `Vec`s is a zip, which `Vec` does not support.
fn main() {
    let a = Vec::from([1, 2]);
    a += Vec::from([3, 4]);
}
