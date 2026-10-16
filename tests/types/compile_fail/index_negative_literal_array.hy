// Expected: E0400 — a negative literal index on a fixed array is out of bounds at compile time (#1173).
fn main() {
    let a = [1, 2];
    let x = a[-1];
}
