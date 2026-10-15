// Expected: E0210 — the second `1` arm can never be reached.
fn f(int n) -> int {
    return match n {
        1 => 1,
        1 => 2,
        default => 0,
    };
}

fn main() {
    let r = f(1);
}
