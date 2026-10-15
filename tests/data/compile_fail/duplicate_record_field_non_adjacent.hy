// Expected: E0208 — non-adjacent duplicate field in record literal (parse E0208).
fn main() {
    let x = { a: 1, b: 2, a: 3 };
}
