// @expected E0119 — `#[test]` is rejected; write `test("…") { … }`.
#[test]
fn check() {
    let x = 1;
}

fn main() {
    check();
}
