// Hit bench for COI-287 W1: dense specialize on float +/−/÷ (no *).
// Pre-W1 infer required MULF/DIVF-as-fmul; add-only stayed fuse-IL.
// DIVF already qualified; this kernel mixes add/sub/div without mul.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf / a - b;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(2.0, 1.0, 2000000) as int)));
}
