// Expected: E0102 — `==` needs both tuples to have the same type, arity included.
fn main() {
    let b = (1, 2) == (1, 2, 3);
}
