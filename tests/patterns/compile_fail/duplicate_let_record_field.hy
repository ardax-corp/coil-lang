// Expected: E0208 — duplicate field in let record pattern (parse E0208).
fn main() {
    let { x: a, x: b } = { x: 1, y: 2 };
}
