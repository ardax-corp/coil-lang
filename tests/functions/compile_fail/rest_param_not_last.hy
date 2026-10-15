// Expected: E0119 — a rest parameter must be the last parameter.
fn tail(int... xs, int a) -> int {
    return a;
}

fn main() {
    let r = tail(1, 2);
}
