// @expected E0001 — `test` needs a name: `test("…") { … }`.
use tests::assert;

test() {
    assert!(true);
}
