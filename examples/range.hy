// Expected output: 012340123561.02.03.0
//
// Lazy Range<T: Ord>: half-open `a..b`, closed `a..=b`, first-class
// values, byte/float bounds. Decreasing ranges are empty (Rust-like).
// Iteration steps by +1 / +1.0 for int/byte/float.
// Numeric ranges also collect with `.to_vec()` — see tests/positive/range_to_vec.hy.

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    // Half-open: 0..5 → 0,1,2,3,4
    for x in 0..5 {
        write_all(stdout(), to_bytes(format("%i", x)));
    }

    // First-class range value
    let r = 0..=3;
    for x in r {
        write_all(stdout(), to_bytes(format("%i", x)));
    }

    // Decreasing is empty — prints nothing
    for x in 10..0 {
        write_all(stdout(), to_bytes(format("%i", x)));
    }

    // Byte range: 5..=6 → 5,6
    let lo: byte = 5;
    let hi: byte = 6;
    for b in lo..=hi {
        write_all(stdout(), to_bytes(format("%i", b)));
    }

    // Float range (Ord + numeric step): 1.0..4.0 → 1.0,2.0,3.0
    for x in 1.0..4.0 {
        write_all(stdout(), to_bytes(format("%f", x)));
    }
}
