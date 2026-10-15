// Expected: E0207 — no parameter is called `c`.
fn diff(int a, int b) -> int {
    return a - b;
}

fn main() {
    let r = diff(a: 1, c: 2);
}
