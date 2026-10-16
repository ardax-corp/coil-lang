// Expected: E0119 — a bound on the module's own `Eq` needs an instance of
// that trait: the builtin `Eq` that `int` has does not count (#1345).
use prelude::ops::Eq as PreludeEq;

trait Eq<T> {
    fn same(T self, T other) -> bool {}
}

fn all_same<T: Eq>(T a, T b) -> bool {
    return a.same(b);
}

fn main() {
    let x = all_same(1, 2);
}
