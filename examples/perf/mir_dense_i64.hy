// Hit bench for COI-288 W2: dense specialize on counted i64 +/− (no heap/CALL).
// Pre-W2 infer required float arith or unused has_i32; i64 loops stayed fuse-IL.
// Trip count is a parameter so the loop is not a const-unroll candidate.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(int a, int b, int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i + a - b;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(2, 1, 2000000))));
}
