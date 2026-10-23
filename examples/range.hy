//
// Lazy Range<T: Ord>: half-open `a..b`, closed `a..=b`, first-class
// values, byte/float bounds. Decreasing ranges are empty (Rust-like).
// Iteration steps by +1 / +1.0 for int/byte/float.
// Numeric ranges also collect with `.to_vec()` — see tests/positive/range_to_vec.hy.
//
// @output 012340123561.02.03.0

use io::sync::print;
use string::format;

fn main() {
    // Half-open: 0..5 → 0,1,2,3,4
    for x in 0..5 {
        let _ = print(format("%i", x));
    }

    // First-class range value
    let r = 0..=3;
    for x in r {
        let _ = print(format("%i", x));
    }

    // Decreasing is empty — prints nothing
    for x in 10..0 {
        let _ = print(format("%i", x));
    }

    // Byte range: 5..=6 → 5,6
    let lo: byte = 5;
    let hi: byte = 6;
    for b in lo..=hi {
        let _ = print(format("%i", b));
    }

    // Float range (Ord + numeric step): 1.0..4.0 → 1.0,2.0,3.0
    for x in 1.0..4.0 {
        let _ = print(format("%f", x));
    }
}
