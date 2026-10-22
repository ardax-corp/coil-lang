// @expected E0119 — `#[test]` on a `fn` is not a test; tests are
// `test("…") { … }` cases.
use tests::assert;

#[test]
fn hidden() {
    assert!(true);
}

fn main() {}
