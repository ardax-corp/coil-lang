// Expected: E0102 — `+` needs both operands of one type; a `byte` and an
// `int` variable do not unify (convert with `as`).
fn main() {
    let b: byte = 1;
    let n = 2;
    let c = b + n;
}
