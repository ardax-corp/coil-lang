// Expected: E0127 — `p` is copied to `q`, so no typed copy of `pair`
// stands in for the value, and its shared body would box `T` inside the
// tuple it returns (#916, #1515).
fn pair<T>(T x) -> (T, int) {
    return (x, 1);
}

fn main() {
    let p = pair;
    let q = p;
    let (a, b) = q(3);
    let _ = a + b;
}
