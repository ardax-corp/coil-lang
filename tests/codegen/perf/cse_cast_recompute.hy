// examples/perf/cse_cast_recompute.hy run as a test: the checksum pins the shape below.
// Output: 1999998000000

// Hit bench for local EarlyCSE: force CastIntToFloat recomputes that
// ssa_gvn does not number (varying i, first cast stored).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn hot(int n) -> int {
    let acc = 0.0;
    let i = 0;
    while i < n {
        let xf = i as float;
        acc = acc + xf;
        acc = acc + (i as float);
        acc = acc + (i as float);
        acc = acc + (i as float);
        i = i + 1;
    }
    return acc as int;
}

fn main() {
    let _ = write_all(stdout(), to_bytes(format("%i", hot(1000000))));
}
