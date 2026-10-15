// Expected: E0102 — a `while` condition must be a `bool`.
fn main() {
    while 42 {
        let x = 1;
    }
}
