// Integer-payload Result allocation: helper RETURNS Result<int, int>.
// No int niche exists yet; each call heap-allocates ObjEnum.
// Explicit return poisons #278 frame-local unboxing (local_escape.rs).
// Heap-payload counterpart: examples/perf/gc_churn.hy.
// ITERS=20000000 (period-10 checksum 19 * 2e6); release VM-only ~1-3s.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

fn main() {
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match checked_div((i % 10) + 1, i % 5) {
            Result::Ok(q) => q,
            Result::Err(e) => e,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
