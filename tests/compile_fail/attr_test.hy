// Expected: compile failure — tests are `test("desc") { … }`, not `#[test]` on fn.
#[test]
fn hidden() {
    assert(true)?;
}

fn main() {}
