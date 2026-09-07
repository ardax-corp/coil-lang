// Hit bench for COI-280 MIR LICM: invariant DIVF each trip of a dense loop.
// Stack-IL leaves a lone `a / b` (not a ≥2-op float chain) in the body;
// after specialize, SSA LICM hoists the divide to the preheader.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let q = a / b;
        s = s + q * xf;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(3.0, 2.0, 4000000) as int)));
}
