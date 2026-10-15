// Expected: E0208 — the same parameter is named twice.
fn diff(int a, int b) -> int {
    return a - b;
}

fn main() {
    let r = diff(a: 1, a: 2);
}
