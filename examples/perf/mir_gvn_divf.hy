// Hit bench for COI-284 MIR cross-block GVN/PRE: `xf / scale` is computed
// in both diamond arms and again after the join. Same-block CSE cannot
// share those; LICM cannot hoist (xf varies). Fork PRE + dominator GVN
// keep one divide per trip (parent executes arm + join).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float scale, float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        if (i & 1) == 0 {
            s = s + (xf / scale) * a;
        } else {
            s = s + (xf / scale) * b;
        }
        s = s + (xf / scale) * (a + b);
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(3.0, 2.0, 4.0, 2000000) as int)));
}
