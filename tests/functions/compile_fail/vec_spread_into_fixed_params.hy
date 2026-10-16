// Expected: E0119 — a `Vec` has no static length to fill fixed parameters.
fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let v = Vec::from([4, 5]);
    let n = add(...v);
}
