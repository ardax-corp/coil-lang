// Expected: E0208 — duplicate field in record literal (parse E0208).
fn main() {
    let x = { foo: 1, foo: 2 };
}
