// Expected: E0208 — a dict literal names each field once.
fn main() {
    let d = { foo: 1, foo: 2 };
}
