// Expected: E0119 — `T: c` with a constraint-kind `c` that no method or operator use
// pins to a concrete trait is unsatisfiable.
fn choose<c: * -> Constraint, T: c>(T a, T b) -> int {
    return 1;
}

fn main() {
    let n = choose(1, 2);
}
