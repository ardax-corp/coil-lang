// Hit bench for COI-283 MIR IV strength reduction: `(i as float) * 7.0`
// becomes add induction. IL SR refuses float affine (not IEEE-exact in
// general); a finite integer-valued const factor is exact for this range.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * 7.0;
        s = s + xf * xf;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(800000) as int)));
}
