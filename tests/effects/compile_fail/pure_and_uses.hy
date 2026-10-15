// Expected: E0001 — a function is `pure` or has a `uses {…}` clause, not both.
pure fn f(int x) -> int uses {read} {
    return x;
}

fn main() {
    f(1);
}
