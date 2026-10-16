// Expected: E0119 — `dot` takes two tuples or two arrays, not one of each.
fn main() {
    let _ = dot((1, 2), [3, 4]);
}
