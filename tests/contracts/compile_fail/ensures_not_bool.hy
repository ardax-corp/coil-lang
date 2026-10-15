// Expected: E0102 — an `ensures` clause must be a `bool`.
fn f(int x) -> int
    ensures result
{
    return x;
}

fn main() {
    f(1);
}
