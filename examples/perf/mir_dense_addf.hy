// Hit bench for COI-287 W1: dense specialize on float +/− (no * or /).
// Pre-W1 infer required MULF/DIVF; add-only stayed fuse-IL. DIVF already
// qualified — this kernel must not use * or / or parent and W1 match.
use io::sync::print;
use string::format;

fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf + a - b;
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", hot(2.0, 1.0, 2000000) as int));
}
