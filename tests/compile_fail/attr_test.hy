// Expected: E0119 — tests are `test("desc") { … }`, not `#[test]` on fn.
#[test]
fn hidden() {
    assert(true)?;
}

fn main() {}
