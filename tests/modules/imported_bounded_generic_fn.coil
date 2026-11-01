// An imported generic function keeps its `Ord` bound: `num::min` and an
// aliased `num::max` work at both `int` and `float` in one caller.
use tests::assert;
use num::{min, max as biggest};

test("an imported bounded generic at several types") {
    assert!(min(3, 1) == 1);
    assert!(min(3.0, 1.5) == 1.5);
    assert!(biggest(3, 9) == 9);
    assert!((typeof min(3, 1)) == "int");
    assert!((typeof min(3.0, 1.0)) == "float");
}
