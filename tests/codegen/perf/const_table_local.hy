// examples/perf/const_table_local.hy run as a test: the checksum pins the shape below.
// Output: 5465

// Lookup-table bench for #1561: a function-local `const` table (the
// PRESENT cipher's 4-bit S-box) read with a variable index, in a function
// called 4M times. The table is built on the first call and loaded after
// that, instead of an array per call.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sbox16(int x) -> int {
    const SBOX = readonly [12, 5, 6, 11, 9, 0, 10, 13, 3, 14, 15, 8, 4, 7, 1, 2];
    let y = 0;
    for k in 0..4 {
        y = y | (SBOX[(x >> (4 * k)) & 15] << (4 * k));
    }
    return y;
}

fn main() {
    let s = 0;
    for i in 0..4000000 {
        s = (s + sbox16(i ^ s)) & 65535;
    }
    let _ = write_all(stdout(), to_bytes(format("%i", s)));
}
