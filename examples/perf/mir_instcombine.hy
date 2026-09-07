// Hit bench for COI-281 MIR InstCombine: `t * 2.0` → `t + t` on dense SSA.
// Fuse-IL algebraic does not strength-reduce float `* 2`; identities on
// binop TOS (`* 1.0`, `+ 0.0`) also fold when they survive the stack window.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * scale;
        let t = xf * xf;
        s = ((s + t * 2.0) * 1.0) + 0.0;
        i = (i + 1) + 0;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(1.5, 1200000) as int)));
}
