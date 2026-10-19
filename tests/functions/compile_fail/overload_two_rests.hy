// Expected: E0121 — two rest-parameter overloads of `f` always overlap.
fn f(int... xs) -> int {
    return xs.len();
}

fn f(string s, int... xs) -> int {
    return xs.len();
}

fn main() {
    let a = f(1, 2);
}
