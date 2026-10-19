// #1345: after `use prelude::ops::Eq as PreludeEq;` a module's own `trait Eq`
// and the builtin both stay usable: the alias still names the builtin as a
// bound, and the short name is the module's trait in bounds (on functions
// and impl blocks) and impls.
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

fn both_equal<T: PreludeEq>(T a, T b) -> bool {
    return a == b;
}

fn all_same<T: Eq>(T a, T b) -> bool {
    return a.same(b);
}

class Pair<T> {
    pub a: T,
    pub b: T,
}

impl Pair<T: Eq> {
    pub fn block_same() -> bool {
        return self.a.same(self.b);
    }
}

test("the alias is still the builtin bound") {
    assert!(both_equal(3, 3));
    assert!(!both_equal("a", "b"));
}

test("the short name is the module's trait") {
    assert!(all_same(new Coin(1), new Coin(1)));
    assert!(!all_same(new Coin(1), new Coin(2)));
    assert!(new Pair(new Coin(3), new Coin(3)).block_same());
}

test("builtin == next to the module's trait") {
    assert!(3 == 3);
    assert!(new Coin(4).same(new Coin(4)));
}
