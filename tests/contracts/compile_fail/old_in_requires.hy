// @expected E0101 — `old(e)` is only available inside `ensures`.
fn f(int x) -> int
    requires old(x) > 0
{
    return x;
}

fn main() {
    f(1);
}
