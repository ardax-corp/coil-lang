// @expected E0136 — inside a `test` body, a statement that drops a `Result`
// is an error, not a warning: calling an `assert!` helper as `helper();`
// would drop its failed `Err` and the case would pass (#1819). Write
// `helper()?;` to fail the case, or `let _ = helper();` to drop it on purpose.
use tests::assert;

fn helper() {
    assert!(1 == 2, "inner");
}

test("a dropped helper Err") {
    helper();
}
