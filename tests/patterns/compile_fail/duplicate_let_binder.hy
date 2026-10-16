// Expected: E0108 — a `let` pattern binds each name once; `(x, x)` repeats `x`.
fn main() {
    let (x, x) = (1, 2);
}
