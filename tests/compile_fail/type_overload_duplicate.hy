// Expected: E0121 — duplicate same-arity same-type overload.
fn f(int x) -> int {
    return x;
}

fn f(int x) -> int {
    return x + 1;
}

fn main() {}
