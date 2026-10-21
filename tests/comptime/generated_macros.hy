// A macro and a derive that a `comptime` block generates are used from
// other modules like hand-written ones.
use comptime_macro_lib::{triple, FieldCount};
use tests::assert;

#[derive(FieldCount)]
class Point {
    pub x: int,
    pub y: int,
}

test("a generated macro") {
    assert!(triple!(2 + 1) == 9);
}

test("a generated derive") {
    assert!(Point::field_count() == 2);
}
