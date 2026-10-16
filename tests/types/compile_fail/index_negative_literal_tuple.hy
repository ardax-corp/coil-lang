// Expected: E0400 — a negative literal index on a tuple is out of bounds at compile time (#1173).
fn main() {
    let t = (1, "a");
    let x = t[-1];
}
