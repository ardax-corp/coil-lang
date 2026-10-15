// Expected: E0119 — a call may not pass more arguments than the function has parameters.
fn one(int a) -> int {
    return a;
}

fn main() {
    let r = one(1, 2);
}
