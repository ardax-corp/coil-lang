// Trait superclasses: `Ordered<T: Equal>` requires `Equal`. A generic bound
// only by `T: Ordered` can still call `eq_val`, through the implied `Equal`
// bound.
//
// @output truetruefalse

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

fn cmp_eq<T: Ordered>(T a, T b) -> bool {
    return eq_val(a, b);
}

fn cmp_lt<T: Ordered>(T a, T b) -> bool {
    return lt_val(a, b);
}

fn main() {
    let _ = print(format("%z", cmp_eq(3, 3)));
    let _ = print(format("%z", cmp_lt(1, 2)));
    let _ = print(format("%z", cmp_eq(1, 2)));
}
