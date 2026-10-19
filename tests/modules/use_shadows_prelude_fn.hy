// An explicit `use m::sin` shadows the prelude `sin`; names not imported
// still resolve to the prelude (#1488).
use tests::assert;
use prelude_clash::{sin, diff, cos as pcos};

test("imported fn shadows prelude sin") {
    assert!(sin(0.0) == 100.0);
}

test("unimported prelude name stays the builtin") {
    assert!(cos(0.0) == 1.0);
    assert!(pcos(0.0) == 200.0);
}

test("imported fn shadows prelude diff") {
    assert!(diff(7, 2) == 5);
}
