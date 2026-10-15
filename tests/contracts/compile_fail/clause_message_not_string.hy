// Expected: E0001 — a clause's message must be a string literal.
fn f(int x) -> int
    requires x > 0, 5
{
    return x;
}

fn main() {
    f(1);
}
