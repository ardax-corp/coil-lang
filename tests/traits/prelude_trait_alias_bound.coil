// #1209: `use prelude::ops::Eq as PreludeEq;` names the builtin `Eq`, so the
// alias works as a generic bound, on a function or on an impl block's
// parameter.
use tests::assert;
use prelude::ops::{Eq as PreludeEq, Ord as Ordered};

fn both_equal<T: PreludeEq>(T a, T b) -> bool {
    return a == b;
}

class Pair<T> {
    pub a: T,
    pub b: T,
}

impl Pair<T: Ordered> {
    pub fn smaller() -> T {
        if self.a < self.b {
            return self.a;
        }
        return self.b;
    }
}

test("an aliased builtin trait is a bound") {
    assert!(both_equal(3, 3));
    assert!(!both_equal("a", "b"));
}

test("an aliased builtin trait bounds an impl block") {
    assert!(new Pair(4, 2).smaller() == 2);
}
