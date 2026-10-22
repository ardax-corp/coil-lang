// @expected E0100 — `result` is bound only in `ensures`, not in `requires`.
fn f(int x) -> int
    requires result > 0
{
    return x;
}

fn main() {
    f(1);
}
