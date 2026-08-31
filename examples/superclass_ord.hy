// Expected output: truetruefalse
//
// Phase 5: trait superclass / implied bounds.
// `Ordered<T: Equal>` stores Equal as a superclass. Dictionary layout is
// flattened (Ordered methods, then Equal methods). A generic with only
// `T: Ordered` can call `eq_val` via the implied Equal bound.

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Equal<T> {
    fn eq_val(T a, T b) -> bool;
}

trait Ordered<T: Equal> {
    fn lt_val(T a, T b) -> bool;
}

impl Equal<int> {
    pub fn eq_val(int a, int b) -> bool {
        return a == b;
    }
}

impl Ordered<int> {
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
    write_all(stdout(), to_bytes(format("%z", cmp_eq(3, 3))));
    write_all(stdout(), to_bytes(format("%z", cmp_lt(1, 2))));
    write_all(stdout(), to_bytes(format("%z", cmp_eq(1, 2))));
}
