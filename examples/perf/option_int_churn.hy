// Integer-payload Option: helper RETURNS Option<int> as two stack slots
// (tag + payload) on direct CALL. Immediate match uses #278 EQ/JMPF.
// Pointer-niche Option<heap> stays one word (see gc_churn.hy).
// ITERS=20000000 (period-10 checksum 42 * 2e6); release VM-only ~1-3s.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn main() {
    let n = 7;
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match lookup(i % 10, n) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
