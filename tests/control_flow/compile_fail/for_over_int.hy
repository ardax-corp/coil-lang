// Expected: E0119 — an `int` is not iterable; iterate a range (`0..n`).
fn main() {
    for x in 5 {
        let y = x;
    }
}
