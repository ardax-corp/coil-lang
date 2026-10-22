// @expected E0102 — a test body returns `Result<(), string>`; it cannot
// `return` a value.
use tests::assert;

test("returns a value") {
    return 5;
}
