// Expected: E0120 — no overload of `f` takes three arguments.
fn f(int a) -> int {
    return a;
}

fn f(int a, int b) -> int {
    return a + b;
}

fn main() {
    let r = f(1, 2, 3);
}
