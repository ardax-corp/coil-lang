// Expected: E0119 — once this module's own trait `Eq` replaces the builtin,
// the alias `PreludeEq` cannot be a bound; the error says why (#1209).
use prelude::ops::Eq as PreludeEq;

trait Eq<T> {
    fn same(T self, T other) -> bool {}
}

fn both_equal<T: PreludeEq>(T a, T b) -> bool {
    return a == b;
}

fn main() {
    let _ = both_equal(3, 3);
}
