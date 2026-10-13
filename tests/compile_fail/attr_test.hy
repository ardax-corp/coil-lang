// Expected: E0119 — tests are `test("desc") { … }`, not `#[test]` on fn.
use tests::assert;

#[test]
fn hidden() {
    assert!(true);
}

fn main() {}
