// Expected: E0121 — two rest-parameter overloads of `f` always overlap.
fn f(int... xs) -> int {
    return len(xs);
}

fn f(string s, int... xs) -> int {
    return len(xs);
}

fn main() {
    let a = f(1, 2);
}
