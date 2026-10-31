// @expected E0119 — a bound `T: c` needs `c: * -> Constraint`; `c: Constraint` is
// ill-kinded.
fn bad<c: Constraint, T: c>(T x) -> T {
    return x;
}

fn main() {}
