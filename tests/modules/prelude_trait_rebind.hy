// Rebinding a prelude name with `use prelude::ops::Eq as PreludeEq;` frees the
// short name for a user trait, and the builtin `==` keeps working.
use tests::assert;
use prelude::ops::Eq as PreludeEq;

trait Eq<T> {
    fn same(T self, T other) -> bool {}
}

class Coin {
    pub cents: int,
}

impl Eq for Coin {
    pub fn same(Coin self, Coin other) -> bool {
        return self.cents == other.cents;
    }
}

fn both_equal(string a, string b) -> bool {
    return a == b;
}

test("the user trait owns the short name") {
    assert!(new Coin(5).same(new Coin(5)));
    assert!(!new Coin(5).same(new Coin(6)));
}

test("the builtin `==` still works on ground types") {
    assert!(3 == 3);
    assert!(both_equal("a", "a"));
    assert!(!both_equal("a", "b"));
}
