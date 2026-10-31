// Macros and a derive that `comptime` blocks (or other macros) generate are
// used from other modules like hand-written ones.
use comptime_macro_lib::{triple, double, FieldCount};
use tests::assert;

#[derive(FieldCount)]
class Point {
    pub x: int,
    pub y: int,
}

test("a generated macro") {
    assert!(triple!(2 + 1) == 9);
}

test("a macro another macro generated") {
    assert!(double!(5) == 10);
}

test("a generated derive") {
    assert!(Point::field_count() == 2);
}
