// Hit bench for COI-282 MIR DestProp: both arms identity-copy `a`, then
// several uses of the dest. InstCombine folds `+ 0` / `* 1`; DestProp
// forwards the join φ so CSE can share `a * xf` (and `t * t` becomes `a * a`).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * b;
        let t = 0.0;
        if xf > a {
            t = a + 0.0;
        } else {
            t = a * 1.0;
        }
        s = s + a * xf + t * xf + t * t;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(1.5, 1.25, 800000) as int)));
}
