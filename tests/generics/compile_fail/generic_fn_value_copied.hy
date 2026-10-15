// Expected: E0127 — `g` is copied to `k`, so no typed copy of `wrap`
// stands in for the value, and its shared body would box `T` (#916).
fn wrap<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn main() {
    let g = wrap;
    let k = g;
    let _ = k(2.5);
}
