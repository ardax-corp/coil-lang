// Numeric tower — Tier B/C: element-generic + shape-generic Num.
// `scale` keeps a fixed `(T,T)` shape; `add` is fully shape-generic and
// monomorphizes to zip when called with ground tuples.
//
// Output: 24,55

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn scale<T: Num>((T, T) v, T s) -> (T, T) {
    return v * s;
}

fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn main() {
    let s = scale((1, 2), 2);
    write_all(stdout(), to_bytes(format("%i%i,", s[0], s[1])));
    let t = add((2, 3), (3, 2));
    write_all(stdout(), to_bytes(format("%i%i", t[0], t[1])));
}
