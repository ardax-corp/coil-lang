// Expected: E0102 — bitwise operators take `int` / `byte` operands, not `float`.
fn main() {
    let x = 1.5 & 2.5;
}
