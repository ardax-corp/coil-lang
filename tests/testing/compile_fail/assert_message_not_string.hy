// @expected E0102 — `assert!`'s message must be a `string`.
use tests::assert;

test("int message") {
    assert!(true, 5);
}
