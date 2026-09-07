// Hit bench for COI-284 MIR cross-block GVN/PRE: `xf / scale` in both
// diamond arms and again after the join. Same-block CSE cannot share
// those; LICM cannot hoist (xf varies). Parent pays two FDIV64s per
// trip; PRE+GVN keep one.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        if (i & 1) == 0 {
            s = s + xf / scale;
        } else {
            s = s + xf / scale;
        }
        s = s + xf / scale;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(3.0, 8000000) as int)));
}
