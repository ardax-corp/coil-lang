// Expected: E0132 — a type-only `let` of a non-scalar type must be assigned before it is read.
fn main() {
    let s: string;
    let t = s;
}
