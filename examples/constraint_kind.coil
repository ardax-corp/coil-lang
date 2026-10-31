//
// Constraint-kind parameter:
// - `c: * -> Constraint` is an abstract unary trait predicate.
// - `T: c` says T is constrained by that predicate.
// - Calling `lt_val` binds `c` to Ordered.
// - Calling `eq_val` then uses Ordered's Equal superclass slot.
//
// @output 42

use io::sync::print;
use string::format;

trait Equal<T> {
    fn eq_val(T a, T b) -> bool {}
}

trait Ordered<T: Equal> {
    fn lt_val(T a, T b) -> bool {}
}

impl Equal for int {
    pub fn eq_val(int a, int b) -> bool {
        return a == b;
    }
}

impl Ordered for int {
    pub fn lt_val(int a, int b) -> bool {
        return a < b;
    }
}

fn choose<c: * -> Constraint, T: c>(T a, T b) -> int {
    if lt_val(a, b) {
        return 0;
    }
    if eq_val(a, b) {
        return 42;
    }
    return 1;
}

fn main() {
    let _ = print(format("%i", choose(7, 7)));
}
