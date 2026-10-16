// #1209: `use prelude::ops::Eq as PreludeEq;` names the builtin `Eq`, so the
// alias works as a generic bound (and in a `where` clause).
use tests::assert;
use prelude::ops::{Eq as PreludeEq, Ord as Ordered};

fn both_equal<T: PreludeEq>(T a, T b) -> bool {
    return a == b;
}

fn smaller<T>(T a, T b) -> T where Ordered<T> {
    if a < b {
        return a;
    }
    return b;
}

test("an aliased builtin trait is a bound") {
    assert!(both_equal(3, 3));
    assert!(!both_equal("a", "b"));
}

test("an aliased builtin trait in a where clause") {
    assert!(smaller(4, 2) == 2);
}
