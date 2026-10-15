// Expected: E0102 — `Vec<int>::push` takes an `int`.
fn main() {
    let v: Vec<int> = Vec::new();
    v.push("a");
}
