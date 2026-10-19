// Expected: E0121 — `f(int x)` and `f(int x, int... xs)` both accept one argument.
fn f(int x) -> int {
    return x;
}

fn f(int x, int... xs) -> int {
    return x + xs.len();
}

fn main() {
    let a = f(1);
}
